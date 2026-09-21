//! Serde plumbing for *open* enums: values that may arrive from a newer Broza.
//!
//! An open enum carries an `Unknown(String)` variant holding the original token, so a
//! value written by a future version survives a read-modify-write cycle unchanged.
//! Closed enums (the ones Broza only ever produces) keep the derived implementations
//! and reject unknown tokens; see [`crate::model`] for which is which.

/// Implement `as_str`, `from_token`, `is_known`, [`std::fmt::Display`] and string
/// serde for an enum whose last variant is `Unknown(String)`.
macro_rules! open_enum {
    ($name:ident { $($variant:ident => $token:literal),+ $(,)? }) => {
        impl $name {
            /// The stable token used in JSON and in the quarantine manifest.
            pub fn as_str(&self) -> &str {
                match self {
                    $( Self::$variant => $token, )+
                    Self::Unknown(raw) => raw.as_str(),
                }
            }

            /// Parse a token. An unrecognised token is preserved verbatim.
            pub fn from_token(raw: &str) -> Self {
                match raw {
                    $( $token => Self::$variant, )+
                    other => Self::Unknown(other.to_owned()),
                }
            }

            /// `true` when this version of Broza understands the value.
            pub fn is_known(&self) -> bool {
                !matches!(self, Self::Unknown(_))
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let raw = <String as serde::Deserialize>::deserialize(deserializer)?;
                Ok(Self::from_token(&raw))
            }
        }
    };
}

pub(crate) use open_enum;
