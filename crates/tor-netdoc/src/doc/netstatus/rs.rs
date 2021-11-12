//! Routerstatus-specific parts of networkstatus parsing.
//!
//! This is a private module; relevant pieces are re-exported by its
//! parent.

#[cfg(feature = "build_docs")]
pub(crate) mod build;
mod md;
mod ns;

use super::{NetstatusKwd, RelayFlags, RelayWeight};
use crate::parse::parser::Section;
use crate::types::misc::*;
use crate::{Error, Result};
use std::{net, time};

use tor_llcrypto::pk::rsa::RsaIdentity;
use tor_protover::Protocols;

pub use md::MdConsensusRouterStatus;
pub use ns::NsConsensusRouterStatus;

/// Shared implementation of MdConsensusRouterStatus and NsConsensusRouterStatus.
#[derive(Debug, Clone)]
struct GenericRouterStatus<D> {
    /// The nickname for this relay.
    ///
    /// Nicknames can be used for convenience purpose, but no more:
    /// there is no mechanism to enforce their uniqueness.
    nickname: String,
    /// Fingerprint of the old-style RSA identity for this relay.
    identity: RsaIdentity,
    /// Declared time at which the router descriptor for this relay
    /// was published.
    ///
    /// This value should be ignored for all purposes; see
    /// [proposal 275](https://gitlab.torproject.org/tpo/core/torspec/-/blob/master/proposals/275-md-published-time-is-silly.txt).
    #[allow(dead_code)] // TODO: remove this some day?
    published: time::SystemTime,
    /// A list of address:port values where this relay can be reached.
    addrs: Vec<net::SocketAddr>,
    /// Declared OR port for this relay.
    #[allow(dead_code)] // This value is never used; we look at addrs instead.
    or_port: u16,
    /// Declared directory port for this relay.
    #[allow(dead_code)] // Arti doesn't use this value.
    dir_port: u16,
    /// Digest of the document for this relay.
    doc_digest: D,
    /// Flags applied by the authorities to this relay.
    flags: RelayFlags,
    /// Version of the software that this relay is running.
    version: Option<String>,
    /// List of subprotocol versions supported by this relay.
    protos: Protocols,
    /// Information about how to weight this relay when choosing a
    /// relay at random.
    weight: RelayWeight,
}

/// Implement a set of accessor functions on a given routerstatus type.
// TODO: These methods should probably become, in whole or in part,
// methods on the RouterStatus trait.
macro_rules! implement_accessors {
    ($name:ident) => {
        impl $name {
            /// Return an iterator of ORPort addresses for this routerstatus
            pub fn orport_addrs(&self) -> impl Iterator<Item = &net::SocketAddr> {
                self.rs.addrs.iter()
            }
            /// Return the declared weight of this routerstatus in the directory.
            pub fn weight(&self) -> &RelayWeight {
                &self.rs.weight
            }
            /// Return the ORPort addresses of this routerstatus
            pub fn addrs(&self) -> &[net::SocketAddr] {
                &self.rs.addrs[..]
            }
            /// Return the protovers that this routerstatus says it implements.
            pub fn protovers(&self) -> &Protocols {
                &self.rs.protos
            }
            /// Return the nickname of this routerstatus.
            pub fn nickname(&self) -> &String {
                &self.rs.nickname
            }
            /// Return the relay flags of this routerstatus.
            pub fn flags(&self) -> &RelayFlags {
                &self.rs.flags
            }
            /// Return the version of this routerstatus.
            pub fn version(&self) -> &Option<String> {
                &self.rs.version
            }
            /// Return true if the ed25519 identity on this relay reflects a
            /// true consensus among the authorities.
            pub fn ed25519_id_is_usable(&self) -> bool {
                !self.rs.flags.contains(RelayFlags::NO_ED_CONSENSUS)
            }
            /// Return true if this routerstatus is listed with the BadExit flag.
            pub fn is_flagged_bad_exit(&self) -> bool {
                self.rs.flags.contains(RelayFlags::BAD_EXIT)
            }
            /// Return true if this routerstatus is listed with the v2dir flag.
            pub fn is_flagged_v2dir(&self) -> bool {
                self.rs.flags.contains(RelayFlags::V2DIR)
            }
            /// Return true if this routerstatus is listed with the Exit flag.
            pub fn is_flagged_exit(&self) -> bool {
                self.rs.flags.contains(RelayFlags::EXIT)
            }
            /// Return true if this routerstatus is listed with the Guard flag.
            pub fn is_flagged_guard(&self) -> bool {
                self.rs.flags.contains(RelayFlags::GUARD)
            }
        }
    };
}

// Make the macro public in the crate.
pub(crate) use implement_accessors;

/// Helper to decode a document digest in the format in which it
/// appears in a given kind of routerstatus.
trait FromRsString: Sized {
    /// Try to decode the given object.
    fn decode(s: &str) -> Result<Self>;
}

impl<D> GenericRouterStatus<D>
where
    D: FromRsString,
{
    /// Parse a generic routerstatus from a section.
    ///
    /// Requires that the section obeys the right SectionRules,
    /// matching microdesc_format.
    fn from_section(
        sec: &Section<'_, NetstatusKwd>,
        microdesc_format: bool,
    ) -> Result<GenericRouterStatus<D>> {
        use NetstatusKwd::*;
        // R line
        let r_item = sec.required(RS_R)?;
        let nickname = r_item.required_arg(0)?.to_string();
        let ident = r_item.required_arg(1)?.parse::<B64>()?;
        let identity = RsaIdentity::from_bytes(ident.as_bytes())
            .ok_or_else(|| Error::BadArgument(r_item.pos(), "Wrong identity length".to_string()))?;
        let skip = if microdesc_format { 0 } else { 1 };
        let published: time::SystemTime = {
            // TODO: It's annoying to have to do this allocation, since we
            // already have a slice that contains both of these arguments.
            // Instead, we could get a slice of arguments: we'd have to add
            // a feature for that.
            let mut p = r_item.required_arg(2 + skip)?.to_string();
            p.push(' ');
            p.push_str(r_item.required_arg(3 + skip)?);
            p.parse::<Iso8601TimeSp>()?.into()
        };
        let ipv4addr = r_item.required_arg(4 + skip)?.parse::<net::Ipv4Addr>()?;
        let or_port = r_item.required_arg(5 + skip)?.parse::<u16>()?;
        let dir_port = r_item.required_arg(6 + skip)?.parse::<u16>()?;

        let mut addrs: Vec<net::SocketAddr> = vec![net::SocketAddr::V4(net::SocketAddrV4::new(
            ipv4addr, or_port,
        ))];

        // A lines
        for a_item in sec.slice(RS_A) {
            addrs.push(a_item.required_arg(0)?.parse::<net::SocketAddr>()?);
        }

        // S line
        let flags = RelayFlags::from_item(sec.required(RS_S)?)?;

        // V line
        let version = sec.maybe(RS_V).args_as_str().map(str::to_string);

        // PR line
        let protos = {
            let tok = sec.required(RS_PR)?;
            tok.args_as_str()
                .parse::<Protocols>()
                .map_err(|e| Error::BadArgument(tok.pos(), e.to_string()))?
        };

        // W line
        let weight = sec
            .get(RS_W)
            .map(RelayWeight::from_item)
            .transpose()?
            .unwrap_or_default();

        // No p line
        // no ID line

        // Try to find the document digest.  This is in different
        // places depending on the kind of consensus we're in.
        let doc_digest: D = if microdesc_format {
            // M line
            let m_item = sec.required(RS_M)?;
            D::decode(m_item.required_arg(0)?)?
        } else {
            D::decode(r_item.required_arg(2)?)?
        };

        Ok(GenericRouterStatus {
            nickname,
            identity,
            published,
            addrs,
            or_port,
            dir_port,
            doc_digest,
            flags,
            version,
            protos,
            weight,
        })
    }
}
