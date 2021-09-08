use crate::array2::{Array, ArrayBuilder, PrimitiveArray, PrimitiveArrayBuilder};
use crate::error::ErrorCode::NumericValueOutOfRange;
use crate::error::{Result, RwError};

use crate::types::NativeType;

/// Checked divide two `PrimitiveArray` for integers. Both array must have the same size.
pub fn vector_div_primitive_integer<T1, T2, T3>(
    a: &PrimitiveArray<T1>,
    b: &PrimitiveArray<T2>,
) -> Result<PrimitiveArray<T3>>
where
    T1: NativeType + num_traits::AsPrimitive<T3>,
    T2: NativeType + num_traits::AsPrimitive<T3>,
    T3: NativeType + num_traits::CheckedDiv,
{
    let mut builder = PrimitiveArrayBuilder::<T3>::new(a.len())?;
    for (a, b) in a.iter().zip(b.iter()) {
        let item = match (a, b) {
            (Some(a), Some(b)) => match a.as_().checked_div(&b.as_()) {
                Some(c) => Some(c),
                None => return Err(RwError::from(NumericValueOutOfRange)),
            },
            _ => None,
        };
        builder.append(item)?;
    }
    builder.finish()
}

/// Checked divide two `PrimitiveArray` for floats. Both array must have the same size.
pub fn vector_div_primitive_float<T1, T2, T3>(
    a: &PrimitiveArray<T1>,
    b: &PrimitiveArray<T2>,
) -> Result<PrimitiveArray<T3>>
where
    T1: NativeType + num_traits::AsPrimitive<T3>,
    T2: NativeType + num_traits::AsPrimitive<T3>,
    T3: NativeType + std::ops::Div + num_traits::Float,
{
    let mut builder = PrimitiveArrayBuilder::<T3>::new(a.len())?;
    for (a, b) in a.iter().zip(b.iter()) {
        let item = match (a, b) {
            (Some(a), Some(b)) => {
                let v = a.as_() / b.as_();
                let check = v.is_finite() && !v.is_nan();
                match check {
                    true => Some(v),
                    false => return Err(RwError::from(NumericValueOutOfRange)),
                }
            }
            _ => None,
        };
        builder.append(item)?;
    }
    builder.finish()
}
