//! Traits for converting keys to and from OpenSSH format.
//
// TODO HSS (#902): OpenSSH keys can have passphrases. While the current implementation isn't able to
// handle such keys, we will eventually need to support them (this will be a breaking API change).

use ssh_key::private::KeypairData;
use ssh_key::Algorithm;

use crate::{ErasedKey, KeyType, KeystoreError, Result};

use tor_error::{internal, ErrorKind, HasKind};
use tor_llcrypto::pk::{curve25519, ed25519};
use zeroize::Zeroizing;

use std::path::PathBuf;
use std::sync::Arc;

/// The algorithm string for x25519 SSH keys.
///
// TODO HSS: start a protocol name registry in the torspec repo and document the usage and purpose
// of this "protocol" name:
//
// ### Assigned Additional Algorithm Names
//
// #### Registration Procedure(s)
//
// TODO
//
// #### NOTE
//
// The algorithm names MUST meet the criteria for additional algorithm names described in [RFC4251
// § 6].
//
// We reserve the following custom OpenSSH key types:
//
//  +---------------------------+--------------------+---------------------+------------------------+
//  | Public Key Algorithm Name | Public Key Format  | Private Key Format  | Purpose                |
//  |---------------------------|--------------------|---------------------|------------------------|
//  | x25519@torproject.org     | [TODO link to spec | [TODO link to spec  | Arti keystore storage  |
//  |                           | describing the key | describing the key  | format                 |
//  |                           | format]            | format]             |                        |
//  |                           |                    |                     |                        |
//  +---------------------------+--------------------+---------------------+------------------------+
//
// [RFC4251 § 6]: https://www.rfc-editor.org/rfc/rfc4251.html#section-6
//
// <The following will go in the document that describes the x25519@torproject.org key format>
//
// # x25519@torproject.org OpenSSH Keys
//
// ## Introduction
//
// X25519 keys do not have a predefined SSH key algorithm name in [IANA's Secure Shell(SSH)
// Protocol Parameters], so in order to be able to store this type of key in OpenSSH format,
// we need to define a custom OpenSSH key type.
//
// ## Key Format
//
// An x25519@torproject.org public key file is encoded in the format specified in
// [RFC4716 § 3.4].
//
// Private keys use the format specified in [PROTOCOL.key].
//
// TODO: flesh out the RFC and write down a concrete example for clarity.
//
// [IANA's Secure Shell(SSH) Protocol Parameters]: https://www.iana.org/assignments/ssh-parameters/ssh-parameters.xhtml#ssh-parameters-19
// [RFC4716 § 3.4]: https://datatracker.ietf.org/doc/html/rfc4716#section-3.4
// [PROTOCOL.key]: https://cvsweb.openbsd.org/src/usr.bin/ssh/PROTOCOL.key?annotate=HEAD
pub(crate) const X25519_ALGORITHM_NAME: &str = "x25519@torproject.org";

/// An unparsed OpenSSH key.
///
/// Note: This is a wrapper around the contents of a file we think is an OpenSSH key. The inner
/// value is unchecked/unvalidated, and might not actually be a valid OpenSSH key.
///
/// The inner value is zeroed on drop.
pub(crate) struct UnparsedOpenSshKey {
    /// The contents of an OpenSSH key file.
    inner: Zeroizing<Vec<u8>>,
    /// The path of the file (for error reporting).
    path: PathBuf,
}

impl UnparsedOpenSshKey {
    /// Create a new [`UnparsedOpenSshKey`].
    ///
    /// The contents of `inner` are erased on drop.
    pub(crate) fn new(inner: Vec<u8>, path: PathBuf) -> Self {
        Self {
            inner: Zeroizing::new(inner),
            path,
        }
    }
}

/// SSH key algorithms.
//
// Note: this contains all the types supported by ssh_key, plus X25519.
#[derive(Clone, Debug, PartialEq, derive_more::Display)]
pub(crate) enum SshKeyAlgorithm {
    /// Digital Signature Algorithm
    Dsa,
    /// Elliptic Curve Digital Signature Algorithm
    Ecdsa,
    /// Ed25519
    Ed25519,
    /// X25519
    X25519,
    /// RSA
    Rsa,
    /// FIDO/U2F key with ECDSA/NIST-P256 + SHA-256
    SkEcdsaSha2NistP256,
    /// FIDO/U2F key with Ed25519
    SkEd25519,
    /// An unrecognized [`ssh_key::Algorithm`].
    Unknown(ssh_key::Algorithm),
}

impl From<Algorithm> for SshKeyAlgorithm {
    fn from(algo: Algorithm) -> SshKeyAlgorithm {
        match algo {
            Algorithm::Dsa => SshKeyAlgorithm::Dsa,
            Algorithm::Ecdsa { .. } => SshKeyAlgorithm::Ecdsa,
            Algorithm::Ed25519 => SshKeyAlgorithm::Ed25519,
            Algorithm::Rsa { .. } => SshKeyAlgorithm::Rsa,
            Algorithm::SkEcdsaSha2NistP256 => SshKeyAlgorithm::SkEcdsaSha2NistP256,
            Algorithm::SkEd25519 => SshKeyAlgorithm::SkEd25519,
            Algorithm::Other(name) if name.as_str() == X25519_ALGORITHM_NAME => {
                SshKeyAlgorithm::X25519
            }
            // Note: ssh_key::Algorithm is non_exhaustive, so we need this catch-all variant
            _ => SshKeyAlgorithm::Unknown(algo),
        }
    }
}

/// An error that occurred while processing an OpenSSH key.
#[derive(thiserror::Error, Debug, Clone)]
pub(crate) enum SshKeyError {
    /// Failed to parse an OpenSSH key
    #[error("Failed to parse OpenSSH with type {key_type:?}")]
    SshKeyParse {
        /// The path of the malformed key.
        path: PathBuf,
        /// The type of key we were trying to fetch.
        key_type: KeyType,
        /// The underlying error.
        #[source]
        err: Arc<ssh_key::Error>,
    },

    /// The OpenSSH key we retrieved is of the wrong type.
    #[error("Unexpected OpenSSH key type: wanted {wanted_key_algo}, found {found_key_algo}")]
    UnexpectedSshKeyType {
        /// The path of the malformed key.
        path: PathBuf,
        /// The algorithm we expected the key to use.
        wanted_key_algo: SshKeyAlgorithm,
        /// The algorithm of the key we got.
        found_key_algo: SshKeyAlgorithm,
    },
}

impl KeystoreError for SshKeyError {}

impl HasKind for SshKeyError {
    fn kind(&self) -> ErrorKind {
        ErrorKind::KeystoreCorrupted
    }
}

impl KeyType {
    /// Get the algorithm of this key type.
    pub(crate) fn ssh_algorithm(&self) -> SshKeyAlgorithm {
        match self {
            KeyType::Ed25519Keypair => SshKeyAlgorithm::Ed25519,
            KeyType::X25519StaticSecret => SshKeyAlgorithm::X25519,
        }
    }

    /// Parse an OpenSSH key, convert the key material into a known key type, and return the
    /// type-erased value.
    ///
    /// The caller is expected to downcast the value returned to a concrete type.
    pub(crate) fn parse_ssh_format_erased(&self, key: UnparsedOpenSshKey) -> Result<ErasedKey> {
        // TODO HSS: perhaps this needs to be a method on EncodableKey instead?

        let key_type = *self;
        let sk = ssh_key::PrivateKey::from_openssh(&*key.inner).map_err(|e| {
            SshKeyError::SshKeyParse {
                // TODO: rust thinks this clone is necessary because key.path is also used below (but
                // if we get to this point, we're going to return an error and never reach the other
                // error handling branches where we use key.path).
                path: key.path.clone(),
                key_type,
                err: e.into(),
            }
        })?;

        let wanted_key_algo = key_type.ssh_algorithm();

        if SshKeyAlgorithm::from(sk.algorithm()) != wanted_key_algo {
            return Err(SshKeyError::UnexpectedSshKeyType {
                path: key.path,
                wanted_key_algo,
                found_key_algo: sk.algorithm().into(),
            }
            .boxed());
        }

        // Build the expected key type (i.e. convert ssh_key key types to the key types
        // we're using internally).
        match sk.key_data() {
            KeypairData::Ed25519(key) => Ok(ed25519::Keypair::from_bytes(&key.to_bytes())
                .map_err(|_| internal!("failed to build ed25519 key out of ed25519 OpenSSH key"))
                .map(Box::new)?),
            KeypairData::Other(key)
                if SshKeyAlgorithm::from(key.algorithm()) == SshKeyAlgorithm::X25519 =>
            {
                let key: [u8; 32] = key
                    .private
                    .as_ref()
                    .try_into()
                    .map_err(|_| internal!("invalid x25519 private key"))?;

                Ok(Box::new(curve25519::StaticSecret::from(key)))
            }
            _ => Err(SshKeyError::UnexpectedSshKeyType {
                path: key.path,
                wanted_key_algo,
                found_key_algo: sk.algorithm().into(),
            }
            .boxed()),
        }
    }
}

#[cfg(test)]
mod tests {
    // @@ begin test lint list maintained by maint/add_warning @@
    #![allow(clippy::bool_assert_comparison)]
    #![allow(clippy::clone_on_copy)]
    #![allow(clippy::dbg_macro)]
    #![allow(clippy::print_stderr)]
    #![allow(clippy::print_stdout)]
    #![allow(clippy::single_char_pattern)]
    #![allow(clippy::unwrap_used)]
    #![allow(clippy::unchecked_duration_subtraction)]
    #![allow(clippy::useless_vec)]
    //! <!-- @@ end test lint list maintained by maint/add_warning @@ -->
    use super::*;

    const OPENSSH_ED25519: &[u8] = include_bytes!("../../testdata/ed25519_openssh.private");
    const OPENSSH_ED25519_BAD: &[u8] = include_bytes!("../../testdata/ed25519_openssh_bad.private");
    const OPENSSH_DSA: &[u8] = include_bytes!("../../testdata/dsa_openssh.private");
    const OPENSSH_X25519: &[u8] = include_bytes!("../../testdata/x25519_openssh.private");
    const OPENSSH_X25519_UNKNOWN_ALGORITHM: &[u8] =
        include_bytes!("../../testdata/x25519_openssh_unknown_algorithm.private");

    #[test]
    fn wrong_key_type() {
        let key_type = KeyType::Ed25519Keypair;
        let key = UnparsedOpenSshKey::new(OPENSSH_DSA.into(), PathBuf::from("/test/path"));
        let err = key_type
            .parse_ssh_format_erased(key)
            .map(|_| "<type erased key>")
            .unwrap_err();

        assert_eq!(
            err.to_string(),
            format!(
                "Unexpected OpenSSH key type: wanted {}, found {}",
                SshKeyAlgorithm::Ed25519,
                SshKeyAlgorithm::Dsa
            )
        );
    }

    #[test]
    fn invalid_ed25519_key() {
        let key_type = KeyType::Ed25519Keypair;
        let key = UnparsedOpenSshKey::new(OPENSSH_ED25519_BAD.into(), PathBuf::from("/test/path"));
        let err = key_type
            .parse_ssh_format_erased(key)
            .map(|_| "<type erased key>")
            .unwrap_err();

        assert_eq!(
            err.to_string(),
            "Failed to parse OpenSSH with type Ed25519Keypair"
        );
    }

    #[test]
    fn ed25519_key() {
        let key_type = KeyType::Ed25519Keypair;
        let key = UnparsedOpenSshKey::new(OPENSSH_ED25519.into(), PathBuf::from("/test/path"));
        let erased_key = key_type.parse_ssh_format_erased(key).unwrap();

        assert!(erased_key.downcast::<ed25519::Keypair>().is_ok());
    }

    #[test]
    fn x25519_key() {
        let key_type = KeyType::X25519StaticSecret;
        let key = UnparsedOpenSshKey::new(OPENSSH_X25519.into(), PathBuf::from("/dummy/path"));
        let erased_key = key_type.parse_ssh_format_erased(key).unwrap();

        assert!(erased_key.downcast::<curve25519::StaticSecret>().is_ok());
    }

    #[test]
    fn invalid_x25519_key() {
        let key_type = KeyType::X25519StaticSecret;
        let key = UnparsedOpenSshKey::new(
            OPENSSH_X25519_UNKNOWN_ALGORITHM.into(),
            PathBuf::from("/dummy/path"),
        );
        let err = key_type
            .parse_ssh_format_erased(key)
            .map(|_| "<type erased key>")
            .unwrap_err();

        assert_eq!(
            err.to_string(),
            "Unexpected OpenSSH key type: wanted X25519, found pangolin@torproject.org"
        );
    }
}
