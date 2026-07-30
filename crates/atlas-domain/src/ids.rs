use serde::{Deserialize, Serialize};
use ts_rs::TS;

macro_rules! string_id {
    ($name:ident) => {
        #[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, TS)]
        #[ts(export)]
        pub struct $name(pub String);

        impl $name {
            #[must_use]
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }
    };
}

string_id!(BlockId);
string_id!(ChapterId);
string_id!(CommandId);
string_id!(DocumentId);
string_id!(JobId);
string_id!(SessionId);
