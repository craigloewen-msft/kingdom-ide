//! Newtyped identifiers.
//!
//! These are all thin wrappers over `String` rather than a single shared `Id`
//! type, so that the compiler rejects passing a [`CityId`] where a
//! [`PlanId`] is expected. Cheap to add now, invasive to add later.

use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(s: impl Into<String>) -> Self {
                Self(s.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_owned())
            }
        }
    };
}

id_type!(
    /// Identifies a project directory within the kingdom.
    CityId
);
id_type!(
    /// Identifies an architectural plan: the unit of work and of review.
    PlanId
);
id_type!(
    /// Identifies a contended machine resource.
    ResourceId
);
