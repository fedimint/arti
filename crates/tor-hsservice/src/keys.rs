//! [`KeySpecifier`] implementations for hidden service keys.

use tor_hscrypto::time::TimePeriod;
use tor_keymgr::{ArtiPath, ArtiPathUnavailableError, CTorPath, KeyPathPattern, KeySpecifier};

use derive_more::Display;

use crate::HsNickname;

/// An identifier for a particular instance of a hidden service key.
#[derive(Clone, Debug, PartialEq)]
pub struct HsSvcKeySpecifier<'a, R: HsSvcKeyRole> {
    /// The nickname of the  hidden service.
    nickname: &'a HsNickname,
    /// The role of this key
    role: R,
    /// The denotators of this key.
    denotator: Option<R::Denotator>,
}

/// An identifier for a particular instance of a hidden service key, and the type of its associated
/// denotators.
pub trait HsSvcKeyRole: Copy + std::fmt::Display + Sealed {
    /// The type of denotator associated with keys that have this key role.
    type Denotator: KeyDenotator;
}

/// Sealed to prevent anything outside this module from implementing `KeyDenotator`.
mod sealed {
    /// Sealed to ensure only the types defined here get to implement `KeyDenotator` and `HsSvcKeyRole`.
    pub trait Sealed {}
}

use sealed::Sealed;

/// A trait for displaying key denotators, for use within an [`ArtiPath`]
/// or [`CTorPath`].
///
/// A key's denotators *denote* an instance of a key.
pub trait KeyDenotator: Sealed {
    /// Display the denotators in a format that can be used within an
    /// [`ArtiPath`] or [`CTorPath`].
    fn display(&self) -> String;

    /// Return a glob pattern that matches the key denotators, if there are any.
    fn glob() -> String;
}

impl Sealed for TimePeriod {}

impl KeyDenotator for TimePeriod {
    fn display(&self) -> String {
        format!(
            "{}_{}_{}",
            self.interval_num(),
            self.length(),
            self.epoch_offset_in_sec()
        )
    }

    fn glob() -> String {
        "*_*_*".into()
    }
}

impl Sealed for () {}

impl KeyDenotator for () {
    fn display(&self) -> String {
        "".into()
    }

    fn glob() -> String {
        "".into()
    }
}

impl<'a, R: HsSvcKeyRole> HsSvcKeySpecifier<'a, R> {
    /// Create a new specifier for service the service with the specified `nickname`.
    pub fn new(nickname: &'a HsNickname, role: R) -> Self {
        Self {
            nickname,
            role,
            denotator: None,
        }
    }

    /// Create a new specifier for service the service with the specified `nickname`,
    /// using the specified `denotators`.
    pub fn with_denotators(nickname: &'a HsNickname, role: R, denotators: R::Denotator) -> Self {
        Self {
            nickname,
            role,
            denotator: Some(denotators),
        }
    }

    /// Get an [`KeyPathPattern`] that can match the [`ArtiPath`]s corresponding to the key
    /// corresponding to the specified service `nickname` and `role`.
    pub(crate) fn arti_pattern(nickname: &HsNickname, role: R) -> KeyPathPattern {
        let pat = Self::arti_path_prefix(nickname, role);
        let glob = R::Denotator::glob();
        KeyPathPattern::new(format!("{pat}_{glob}"))
    }
}

/// A key role for hidden service identity keys.
#[derive(Debug, Clone, Copy, PartialEq, Display)]
#[non_exhaustive]
pub enum HsSvcHsIdKeyRole {
    /// The public part of the identity key of the service.
    #[display(fmt = "KP_hs_id")]
    HsIdPublicKey,
    /// The long-term identity keypair of the service.
    #[display(fmt = "KS_hs_id")]
    HsIdKeypair,
}

impl Sealed for HsSvcHsIdKeyRole {}

impl HsSvcKeyRole for HsSvcHsIdKeyRole {
    type Denotator = ();
}

/// A key role for keys that have `TimePeriod` metadata.
#[derive(Debug, Clone, Copy, PartialEq, Display)]
#[non_exhaustive]
pub enum HsSvcKeyRoleWithTimePeriod {
    /// The blinded signing keypair.
    #[display(fmt = "KS_hs_blind_id")]
    BlindIdKeypair,
    /// The public part of the blinded signing keypair.
    #[display(fmt = "KP_hs_blind_id")]
    BlindIdPublicKey,
    /// The descriptor signing key.
    #[display(fmt = "KS_hs_desc_sign")]
    DescSigningKeypair,
}

impl Sealed for HsSvcKeyRoleWithTimePeriod {}

impl HsSvcKeyRole for HsSvcKeyRoleWithTimePeriod {
    type Denotator = TimePeriod;
}

impl<'a, R: HsSvcKeyRole> HsSvcKeySpecifier<'a, R> {
    /// Returns the prefix of the [`ArtiPath`] corresponding to the `HsSvcKeySpecifier` with the
    /// specified `nickname` and `role`, containing the service nickname and the key role (but not
    /// the key metadata).
    pub(crate) fn arti_path_prefix(nickname: &HsNickname, role: R) -> String {
        format!("hs/{nickname}/{role}")
    }
}

impl<'a, R: HsSvcKeyRole> KeySpecifier for HsSvcKeySpecifier<'a, R> {
    fn arti_path(&self) -> Result<ArtiPath, ArtiPathUnavailableError> {
        let prefix = Self::arti_path_prefix(self.nickname, self.role);
        let path = match &self.denotator {
            // TODO HSS: use a different character to separate the key name from the metadata
            // See arti#1063.
            Some(meta) => ArtiPath::new(format!("{prefix}_{}", meta.display())),
            None => ArtiPath::new(prefix),
        }
        .map_err(|e| tor_error::internal!("{e}"))?;

        Ok(path)
    }

    fn ctor_path(&self) -> Option<CTorPath> {
        // TODO HSS: the HsSvcKeySpecifier will need to be configured with all the directories used
        // by C tor. The resulting CTorPath will be prefixed with the appropriate C tor directory,
        // based on the HsSvcKeyRole.
        //
        // This function will return `None` for keys that aren't stored on disk by C tor.
        todo!()
    }
}
