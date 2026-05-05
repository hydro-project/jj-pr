use std::borrow::Borrow;
use std::fmt::Display;
use std::ops::Deref;

use ref_cast::RefCast;
use serde::{Deserialize, Serialize};

/// Generates a transparent newtype over a string-like inner type with `Display`,
/// `Deref`, `Borrow`, and `ToOwned` impls (mirroring the `str`/`String` pattern).
macro_rules! newtype_str {
    (
        $(
            $( #[$meta:meta] )* $vis:vis $Name:ident;
        )*
    ) => {
        $(
            $( #[$meta] )*
            #[derive(Clone, Debug, Deserialize, Serialize, Eq, Hash, Ord, PartialEq, PartialOrd, RefCast)]
            #[repr(transparent)]
            #[serde(transparent)]
            $vis struct $Name<T: ?Sized = String>(pub T);

            impl<T: ?Sized + Display> Display for $Name<T> {
                fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    self.0.fmt(f)
                }
            }

            impl<T: ?Sized + Deref> Deref for $Name<T> {
                type Target = $Name<T::Target>;
                fn deref(&self) -> &Self::Target {
                    $Name::ref_cast(self.0.deref())
                }
            }

            impl Borrow<$Name<str>> for $Name {
                fn borrow(&self) -> &$Name<str> {
                    self
                }
            }

            impl ToOwned for $Name<str> {
                type Owned = $Name<String>;
                fn to_owned(&self) -> Self::Owned {
                    $Name(self.0.to_owned())
                }
            }

            impl PartialEq<str> for $Name {
                fn eq(&self, other: &str) -> bool {
                    self.0 == other
                }
            }

            impl PartialEq<&str> for $Name {
                fn eq(&self, other: &&str) -> bool {
                    self.0 == *other
                }
            }

            impl PartialEq<$Name> for &str {
                fn eq(&self, other: &$Name) -> bool {
                    *self == other.0
                }
            }

            impl $Name<str> {
                /// Returns the inner `&str`.
                pub fn as_str(&self) -> &str {
                    &self.0
                }
            }
        )*
    };
}

newtype_str! {
    /// A `git`/`jj` commit SHA hash. Used by both `jj` and GitHub.
    pub CommitId;

    /// A `jj` change ID.
    pub ChangeId;

    /// A bookmark (branch) name.
    pub Bookmark;
}
