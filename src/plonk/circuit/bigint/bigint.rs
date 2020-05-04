use crate::bellman::pairing::{
    Engine,
};

use crate::bellman::pairing::ff::{
    Field,
    PrimeField,
    PrimeFieldRepr,
    BitIterator
};

use crate::bellman::{
    SynthesisError,
};

use crate::bellman::plonk::better_better_cs::cs::{
    Variable, 
    ConstraintSystem,
    ArithmeticTerm,
    MainGateTerm,
    Width4MainGateWithDNextEquation,
    MainGateEquation,
    GateEquationInternal,
    GateEquation,
    LinearCombinationOfTerms,
    PolynomialMultiplicativeTerm,
    PolynomialInConstraint,
    TimeDilation,
    Coefficient,
    PlonkConstraintSystemParams
};

use num_bigint::BigUint;

use super::super::allocated_num::{AllocatedNum, Num};
use super::super::linear_combination::LinearCombination;
use super::super::simple_term::Term;

use super::{U16RangeConstraintinSystem, constraint_num_bits};

// in principle this is valid for both cases:
// when we represent some (field) element as a set of limbs
// that are power of two, or if it's a single element as in RNS

#[derive(Clone, Debug)]
pub struct LimbedRepresentationParameters<E: Engine> {
    pub limb_size_bits: usize,
    pub limb_max_value: BigUint,
    pub limb_max_intermediate_value: BigUint,
    pub limb_intermediate_value_capacity: usize,
    pub shift_left_by_limb_constant: E::Fr,
    pub shift_right_by_limb_constant: E::Fr,
    pub mul_two_constant: E::Fr,
    pub div_two_constant: E::Fr
}

impl<E: Engine> LimbedRepresentationParameters<E> {
    pub fn new(limb_size: usize, intermediate_value_capacity: usize) -> Self {
        // assert!(limb_size <= (E::Fr::CAPACITY as usize) / 2);
        // assert!(intermediate_value_capacity <= E::Fr::CAPACITY as usize);

        let limb_max_value = (BigUint::from(1u64) << limb_size) - BigUint::from(1u64);

        let tmp = BigUint::from(1u64) << limb_size;

        let shift_left_by_limb_constant = E::Fr::from_str(&tmp.to_string()).unwrap();

        let shift_right_by_limb_constant = shift_left_by_limb_constant.inverse().unwrap();

        let mut two = E::Fr::one();
        two.double();

        let div_two_constant = two.inverse().unwrap();

        Self {
            limb_size_bits: limb_size,
            limb_max_value,
            limb_max_intermediate_value: (BigUint::from(1u64) << intermediate_value_capacity) - BigUint::from(1u64),
            limb_intermediate_value_capacity: intermediate_value_capacity,
            shift_left_by_limb_constant,
            shift_right_by_limb_constant,
            mul_two_constant: two,
            div_two_constant,
        }
    }
}

// Simple term and bit counter/max value counter that we can update
#[derive(Clone, Debug)]
pub struct Limb<E: Engine> {
    pub term: Term<E>,
    pub max_value: BigUint,
}

pub(crate) fn get_num_bits<F: PrimeField>(el: &F) -> usize {
    let repr = el.into_repr();
    let mut num_bits = repr.as_ref().len() * 64;
    for &limb in repr.as_ref().iter().rev() {
        if limb == 0 {
            num_bits -= 64;
        } else {
            num_bits -= limb.leading_zeros() as usize;
            break;
        }
    }

    num_bits
}

impl<E: Engine> Limb<E> {
    pub fn new(
        term: Term<E>,
        max_value: BigUint,
    ) -> Self {
        Self {
            term,
            max_value,
        }
    }

    pub fn max_bits(&mut self) -> usize {
        self.max_value.bits() + 1
    }

    pub fn inc_max(&mut self, by: &BigUint) {
        self.max_value += by;
    }

    pub fn scale_max(&mut self, by: &BigUint) {
        self.max_value *= by;
    }

    pub fn max_value(&self) -> BigUint {
        self.max_value.clone()
    }

    pub fn get_value(&self) -> BigUint {
        fe_to_biguint(&self.term.get_value().unwrap())
    }

    pub fn scale(&mut self, by: &E::Fr) {
        self.term.scale(by);
    }

    pub fn negate(&mut self) {
        self.term.negate();
    }

    pub fn add_constant(&mut self, c: &E::Fr) {
        self.term.add_constant(&c);
    }

    pub fn get_field_value(&self) -> E::Fr {
        debug_assert!(self.get_value() < repr_to_biguint::<E::Fr>(&E::Fr::char()), "self value = {}, char = {}", self.get_value().to_str_radix(16), E::Fr::char());

        let v = self.term.get_value().unwrap();

        v
    }

    pub fn is_constant(&self) -> bool {
        self.term.is_constant()
    }

    pub fn collapse_into_constant(&self) -> E::Fr {
        self.term.get_constant_value()
    }

    pub fn collapse_into_num<CS: ConstraintSystem<E>>(
        &self,
        cs: &mut CS
    ) -> Result<Num<E>, SynthesisError> {
        self.term.collapse_into_num(cs)
    }
}

pub(crate) fn repr_to_biguint<F: PrimeField>(repr: &F::Repr) -> BigUint {
    let mut b = BigUint::from(0u64);
    for &limb in repr.as_ref().iter().rev() {
        b <<= 64;
        b += BigUint::from(limb)
    }

    b
}

#[inline]
pub fn mod_inverse(el: &BigUint, modulus: &BigUint) -> BigUint {
    use crate::num_bigint::BigInt;
    use crate::num_integer::{Integer, ExtendedGcd};
    use crate::num_traits::{ToPrimitive, Zero, One};

    if el.is_zero() {
        panic!("division by zero");
    }

    let el_signed = BigInt::from(el.clone());
    let modulus_signed = BigInt::from(modulus.clone());

    let ExtendedGcd{ gcd, x: _, y, .. } = modulus_signed.extended_gcd(&el_signed); 
    assert!(gcd.is_one());
    let y = if y < BigInt::zero() {
        let mut y = y;
        y += modulus_signed;

        y.to_biguint().expect("must be > 0")
    } else {
        y.to_biguint().expect("must be > 0")
    };

    debug_assert!(&y < modulus);

    y
}

pub(crate) fn biguint_to_fe<F: PrimeField>(value: BigUint) -> F {
    F::from_str(&value.to_str_radix(10)).unwrap()
}

pub(crate) fn fe_to_biguint<F: PrimeField>(el: &F) -> BigUint {
    let repr = el.into_repr();

    repr_to_biguint::<F>(&repr)
}

// pub(crate) fn fe_to_raw_biguint<F: PrimeField>(el: &F) -> BigUint {
//     let repr = el.into_raw_repr();

//     repr_to_biguint::<F>(&repr)
// }

// pub(crate) fn fe_to_mont_limbs<F: PrimeField>(el: &F, bits_per_limb: usize) -> Vec<BigUint> {
//     let repr = el.into_raw_repr();

//     let fe = repr_to_biguint::<F>(&repr);

//     split_into_fixed_width_limbs(fe, bits_per_limb)
// }   

pub fn split_into_fixed_width_limbs(mut fe: BigUint, bits_per_limb: usize) -> Vec<BigUint> {
    let mut num_limbs = fe.bits() / bits_per_limb;
    if fe.bits() % bits_per_limb != 0 {
        num_limbs += 1;
    }

    let mut limbs = Vec::with_capacity(num_limbs);

    let modulus = BigUint::from(1u64) << bits_per_limb;

    for _ in 0..num_limbs {
        let limb = fe.clone() % &modulus;
        limbs.push(limb);
        fe >>= bits_per_limb;
    }

    limbs.reverse();

    limbs
}


pub fn split_into_fixed_number_of_limbs(mut fe: BigUint, bits_per_limb: usize, num_limbs: usize) -> Vec<BigUint> {
    let mut limbs = Vec::with_capacity(num_limbs);

    let modulus = BigUint::from(1u64) << bits_per_limb;

    for _ in 0..num_limbs {
        let limb = fe.clone() % &modulus;
        limbs.push(limb);
        fe >>= bits_per_limb;
    }

    limbs
}

pub struct LimbedBigUint<'a, E: Engine> {
    pub(crate) params: &'a LimbedRepresentationParameters<E>,
    pub(crate) num_limbs: usize,
    pub(crate) limbs: Vec<Limb<E>>,
    pub(crate) is_constant: bool
}

impl<'a, E: Engine> LimbedBigUint<'a, E> {
    pub fn get_value(&self) -> BigUint {
        let shift = self.params.limb_size_bits;

        let mut result = BigUint::from(0u64);

        for l in self.limbs.iter().rev() {
            result <<= shift;
            result += l.get_value();
        }

        result
    }

    // pub fn reduce_if_necessary<CS: ConstraintSystem<E>>(
    //     &mut self,
    //     cs: &mut CS
    // ) -> Result<(), SynthesisError> {
    //     if self.is_constant {

    //     }
    // }
}


