//! pgvector's `vector` on the sqlx the queries run on. A query's field is the crate's own
//! `pgvector::Vector`, of whatever version its `Cargo.toml` names, made from the `Vec<f32>` of
//! [`PgVector`] with `From` and giving it back with `.to_vec()`, which every release of the crate
//! has, while [`PgVector`] reads and writes the type's binary form itself. So reylo
//! depends on no version of the `pgvector` crate, and none of that crate's sqlx support, which may
//! resolve to another sqlx than the project's, is in the path.

use crate::sqlx::encode::IsNull;
use crate::sqlx::error::BoxDynError;
use crate::sqlx::postgres::{PgArgumentBuffer, PgHasArrayType, PgTypeInfo, PgValueFormat, PgValueRef, Postgres};
use crate::sqlx::{Decode, Encode, Type, TypeInfo};

/// A vector's values, as a query decodes and binds them.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq)]
pub struct PgVector(pub Vec<f32>);

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
        Ok(PgVector(values))
    }
}

impl Encode<'_, Postgres> for PgVector {
    fn encode_by_ref(&self, buf: &mut PgArgumentBuffer) -> Result<IsNull, BoxDynError> {
        let values = &self.0;
        let dimensions = u16::try_from(values.len()).map_err(|_| format!("a vector of {} dimensions is past pgvector's 16,000", values.len()))?;
        buf.extend_from_slice(&dimensions.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());
        for v in values {
            buf.extend_from_slice(&v.to_be_bytes());
        }
        Ok(IsNull::No)
    }
}
