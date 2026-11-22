//! pgvector's `vector` on the sqlx the queries run on. A query's field is the `pgvector` crate's
//! `Vector`, decoded and bound through [`PgVector`], which reads and writes the type's binary form
//! itself: the `pgvector` crate's own sqlx support takes sqlx 0.8 or 0.9, and Cargo may resolve it
//! to another sqlx than the project's, whose traits a query cannot use.

use crate::sqlx::encode::IsNull;
use crate::sqlx::error::BoxDynError;
use crate::sqlx::postgres::{PgArgumentBuffer, PgHasArrayType, PgTypeInfo, PgValueFormat, PgValueRef, Postgres};
use crate::sqlx::{Decode, Encode, Type, TypeInfo};

/// A `pgvector::Vector`, as a query decodes and binds it.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq)]
pub struct PgVector(pub pgvector::Vector);

impl Type<Postgres> for PgVector {
    fn type_info() -> PgTypeInfo {
        PgTypeInfo::with_name("vector")
    }

    fn compatible(ty: &PgTypeInfo) -> bool {
        ty.name() == "vector"
    }
}

impl PgHasArrayType for PgVector {
    fn array_type_info() -> PgTypeInfo {
        PgTypeInfo::with_name("_vector")
    }

    fn array_compatible(ty: &PgTypeInfo) -> bool {
        ty.name() == "_vector" || ty.name() == "vector[]"
    }
}

impl<'r> Decode<'r, Postgres> for PgVector {
    fn decode(value: PgValueRef<'r>) -> Result<Self, BoxDynError> {
        let values = match value.format() {
            // The dimension count, a reserved u16, then each element as a big-endian f32.
            PgValueFormat::Binary => {
                let bytes = value.as_bytes()?;
                let (head, rest) = bytes.split_at_checked(4).ok_or("a vector's binary value is shorter than its header")?;
                let dimensions = usize::from(u16::from_be_bytes([head[0], head[1]]));
                if rest.len() != dimensions * 4 {
                    return Err(format!("a vector of {dimensions} dimensions arrived with {} bytes of values", rest.len()).into());
                }
                rest.chunks_exact(4).map(|c| f32::from_be_bytes([c[0], c[1], c[2], c[3]])).collect()
            }
            // `[1,2.5,-3]`, which pgvector writes as the shortest text that reads back as the same f32.
            PgValueFormat::Text => {
                let text = value.as_str()?;
                let inner = text.strip_prefix('[').and_then(|t| t.strip_suffix(']')).ok_or_else(|| format!("{text:?} is no vector"))?;
                inner.split(',').map(|v| v.trim().parse::<f32>().map_err(|_| format!("{text:?} is no vector"))).collect::<Result<Vec<f32>, String>>()?
            }
        };
        Ok(PgVector(pgvector::Vector::from(values)))
    }
}

impl Encode<'_, Postgres> for PgVector {
    fn encode_by_ref(&self, buf: &mut PgArgumentBuffer) -> Result<IsNull, BoxDynError> {
        let values = self.0.as_slice();
        let dimensions = u16::try_from(values.len()).map_err(|_| format!("a vector of {} dimensions is past pgvector's 16,000", values.len()))?;
        buf.extend_from_slice(&dimensions.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());
        for v in values {
            buf.extend_from_slice(&v.to_be_bytes());
        }
        Ok(IsNull::No)
    }
}

/// A value of a `Params` field the crate declares for a vector parameter, as it is bound: a
/// `pgvector::Vector` (in an `Option` or a `Vec` too) through [`PgVector`], whatever name the crate
/// gave the type, and a value of any other type as it is. `(&&Bind(v)).reylo_bind()` takes
/// [`BindVector`]'s method where it applies, and [`BindAsIs`]'s one dereference further on.
#[doc(hidden)]
pub struct Bind<T>(pub T);

#[doc(hidden)]
pub trait BindVector {
    type Bound;
    fn reylo_bind(self) -> Self::Bound;
}

impl BindVector for &&Bind<pgvector::Vector> {
    type Bound = PgVector;
    fn reylo_bind(self) -> PgVector {
        PgVector(self.0.clone())
    }
}

impl BindVector for &&Bind<Option<pgvector::Vector>> {
    type Bound = Option<PgVector>;
    fn reylo_bind(self) -> Option<PgVector> {
        self.0.clone().map(PgVector)
    }
}

impl BindVector for &&Bind<Vec<pgvector::Vector>> {
    type Bound = Vec<PgVector>;
    fn reylo_bind(self) -> Vec<PgVector> {
        self.0.iter().cloned().map(PgVector).collect()
    }
}

#[doc(hidden)]
pub trait BindAsIs<T> {
    fn reylo_bind(self) -> T;
}

impl<T: Clone> BindAsIs<T> for &Bind<T> {
    fn reylo_bind(self) -> T {
        self.0.clone()
    }
}
