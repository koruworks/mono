// SPDX-License-Identifier: MPL-2.0

//! A site, a starting address, and what an address is reduced to.
//!
//! A **site** ([`BrowserOrigin`]) is `scheme://host` or `scheme://host:port`
//! in one canonical spelling, so two sites are the same site exactly when
//! their text is equal. There is no wildcard.
//!
//! A **starting address** ([`StartingAddress`]) is a full address a person
//! wrote. It is kept byte for byte, because it is opened exactly as listed.
//!
//! Every type here that holds a site or an address has a `Debug` that shows
//! neither, and none has a `Display`: the text is reached only through
//! `as_str`, by code that means to use it. An error is a closed token and
//! never carries the text that was refused.

use std::fmt;
use std::net::Ipv4Addr;

use url::{Host, Url};

/// `https://` (8) + a 253-byte host name + `:` + a five-digit port.
pub const MAX_ORIGIN_BYTES: usize = 267;
/// The longest starting address a person may list.
pub const MAX_STARTING_ADDRESS_BYTES: usize = 2048;
/// The longest address of a page or a link kept in memory for the current
/// observation. A longer one is reduced to its site and its text is dropped.
pub const MAX_PAGE_ADDRESS_BYTES: usize = 8192;

/// Why a site or an address was refused. A token only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressError {
    /// Over the byte bound.
    TooLong,
    /// Not an address at all, or it holds a space, a control character or a
    /// backslash, each of which a browser may read differently from Koru.
    Malformed,
    /// A scheme other than `https` (or loopback `http`).
    Scheme,
    /// `http` to anything but `127.0.0.1` or `localhost`.
    PlainHttp,
    /// A user name or a password in the address.
    UserName,
    /// No host.
    Host,
    /// A site was written with a path.
    Path,
    /// A site was written with a query.
    Query,
    /// A site was written with a fragment.
    Fragment,
}

/// One site, canonical: lower-case ASCII host, default port omitted.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BrowserOrigin(String);

impl BrowserOrigin {
    /// A site as a person or a stored grant spells it: `scheme://host` with
    /// an optional port and nothing after it. Case, a default port and an
    /// international name are brought to the canonical spelling; a path
    /// (even a lone `/`), a query, a fragment or a user name is refused.
    pub fn parse(text: &str) -> Result<Self, AddressError> {
        let url = checked(text, MAX_STARTING_ADDRESS_BYTES)?;
        if url.query().is_some() {
            return Err(AddressError::Query);
        }
        if url.fragment().is_some() {
            return Err(AddressError::Fragment);
        }
        if url.path() != "/" || after_scheme(text).contains('/') {
            return Err(AddressError::Path);
        }
        origin_of(&url)
    }

    /// The canonical text. The only way to it.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for BrowserOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BrowserOrigin(..)")
    }
}

/// A full address a person listed, exactly as they wrote it, and its site.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct StartingAddress {
    text: String,
    origin: BrowserOrigin,
}

impl StartingAddress {
    /// The same scheme rule as a site, no user name, at most
    /// [`MAX_STARTING_ADDRESS_BYTES`]. A path, a query and a fragment are the
    /// person's to choose. Nothing is trimmed or rewritten.
    pub fn parse(text: &str) -> Result<Self, AddressError> {
        let url = checked(text, MAX_STARTING_ADDRESS_BYTES)?;
        Ok(Self {
            origin: origin_of(&url)?,
            text: text.to_owned(),
        })
    }

    /// A site entered as a bare name: its starting address is
    /// `https://<name>/`, in the name's canonical spelling because Koru, not
    /// the person, wrote the rest of it.
    pub fn for_site_name(name: &str) -> Result<Self, AddressError> {
        if name.is_empty() || name.contains(['/', '?', '#', '@', ':']) {
            return Err(AddressError::Malformed);
        }
        let origin = BrowserOrigin::parse(&format!("https://{name}"))?;
        Ok(Self {
            text: format!("{}/", origin.as_str()),
            origin,
        })
    }

    /// The address, byte for byte as listed.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn origin(&self) -> &BrowserOrigin {
        &self.origin
    }
}

impl fmt::Debug for StartingAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StartingAddress(..)")
    }
}

/// What an address that is not a grantable site is reduced to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UngrantableSite {
    /// The empty page a new window opens on.
    Blank,
    /// `http` to a host that is not loopback.
    PlainHttp,
    /// Any other scheme: a browser's own pages, `data:`, `blob:`, `file:`.
    OtherScheme,
    /// Not an address, or one too long to keep.
    Unparsed,
}

impl UngrantableSite {
    pub fn token(self) -> &'static str {
        match self {
            Self::Blank => "blank",
            Self::PlainHttp => "plain_http",
            Self::OtherScheme => "other_scheme",
            Self::Unparsed => "unparsed",
        }
    }
}

/// An address reduced to its site: all that crosses the decode boundary for
/// anything but the current page and the links of the current observation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AddressSite {
    /// A site a grant could name.
    Grantable(BrowserOrigin),
    /// Anything else, as a token. A policy refuses all of these.
    Ungrantable(UngrantableSite),
}

impl AddressSite {
    /// Reduce an address a browser reported. Never fails: what cannot be
    /// read is a token.
    pub fn of(address: &str) -> Self {
        if address == "about:blank" {
            return Self::Ungrantable(UngrantableSite::Blank);
        }
        if address.len() > MAX_PAGE_ADDRESS_BYTES {
            return Self::Ungrantable(UngrantableSite::Unparsed);
        }
        let Ok(url) = Url::parse(address) else {
            return Self::Ungrantable(UngrantableSite::Unparsed);
        };
        match origin_of(&url) {
            Ok(origin) => Self::Grantable(origin),
            Err(AddressError::PlainHttp) => Self::Ungrantable(UngrantableSite::PlainHttp),
            Err(AddressError::Scheme) => Self::Ungrantable(UngrantableSite::OtherScheme),
            Err(_) => Self::Ungrantable(UngrantableSite::Unparsed),
        }
    }

    pub fn origin(&self) -> Option<&BrowserOrigin> {
        match self {
            Self::Grantable(origin) => Some(origin),
            Self::Ungrantable(_) => None,
        }
    }

    /// The reduced form a qualification transcript records: the site's text,
    /// or the token. This is the one place a site is written out, and only
    /// the qualification program calls it.
    pub fn record(&self) -> &str {
        match self {
            Self::Grantable(origin) => origin.as_str(),
            Self::Ungrantable(token) => token.token(),
        }
    }
}

/// The full address of the current page, or of one link of the current
/// observation. Held in memory only, never logged, never stored.
#[derive(Clone, PartialEq, Eq)]
pub struct PageAddress {
    text: String,
    site: AddressSite,
}

impl PageAddress {
    /// An address a browser reported. `None` when it is too long to keep or
    /// is not an address; its site is then all there is (`AddressSite::of`).
    pub fn reported(address: &str) -> Option<Self> {
        if address.len() > MAX_PAGE_ADDRESS_BYTES || Url::parse(address).is_err() {
            return None;
        }
        Some(Self {
            site: AddressSite::of(address),
            text: address.to_owned(),
        })
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn site(&self) -> &AddressSite {
        &self.site
    }

    /// The path alone: no query and no fragment.
    pub fn path(&self) -> String {
        Url::parse(&self.text)
            .map(|url| url.path().to_owned())
            .unwrap_or_default()
    }
}

impl From<&StartingAddress> for PageAddress {
    fn from(address: &StartingAddress) -> Self {
        Self {
            text: address.text.clone(),
            site: AddressSite::Grantable(address.origin.clone()),
        }
    }
}

impl fmt::Debug for PageAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PageAddress(..)")
    }
}

/// What follows `scheme://` in the text as written.
fn after_scheme(text: &str) -> &str {
    text.split_once("://").map_or("", |(_, rest)| rest)
}

/// The checks every written address passes before its site is taken.
fn checked(text: &str, limit: usize) -> Result<Url, AddressError> {
    if text.len() > limit {
        return Err(AddressError::TooLong);
    }
    // The `url` crate drops tabs and newlines, trims spaces and reads a
    // backslash as a slash. A browser may not agree, so none is accepted.
    if text
        .bytes()
        .any(|byte| byte <= 0x20 || byte == 0x7f || byte == b'\\')
    {
        return Err(AddressError::Malformed);
    }
    let url = Url::parse(text).map_err(|_| AddressError::Malformed)?;
    if !matches!(url.scheme(), "https" | "http") {
        return Err(AddressError::Scheme);
    }
    let authority = after_scheme(text);
    let authority = authority
        .find(['/', '?', '#'])
        .map_or(authority, |end| &authority[..end]);
    if !url.username().is_empty() || url.password().is_some() || authority.contains('@') {
        return Err(AddressError::UserName);
    }
    Ok(url)
}

/// The canonical site of a parsed address, under the scheme rule.
fn origin_of(url: &Url) -> Result<BrowserOrigin, AddressError> {
    let secure = match url.scheme() {
        "https" => true,
        "http" => false,
        _ => return Err(AddressError::Scheme),
    };
    let host = url.host().ok_or(AddressError::Host)?;
    let loopback = match &host {
        // A name is letters, digits, hyphens, dots and underscores once it
        // is in its ASCII form. The address rules allow more (a `*`, say),
        // and none of it is a name a person means.
        Host::Domain(name)
            if !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_')) =>
        {
            return Err(AddressError::Host);
        }
        Host::Domain(name) => *name == "localhost",
        Host::Ipv4(address) => *address == Ipv4Addr::LOCALHOST,
        Host::Ipv6(_) => false,
    };
    if !secure && !loopback {
        return Err(AddressError::PlainHttp);
    }
    // `host_str` is the lower-case ASCII form, an IPv6 host in brackets.
    // `port` is `None` for the scheme's default port.
    let name = url.host_str().ok_or(AddressError::Host)?;
    if name.is_empty() {
        return Err(AddressError::Host);
    }
    let text = match url.port() {
        Some(port) => format!("{}://{name}:{port}", url.scheme()),
        None => format!("{}://{name}", url.scheme()),
    };
    if text.len() > MAX_ORIGIN_BYTES {
        return Err(AddressError::TooLong);
    }
    Ok(BrowserOrigin(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(text: &str) -> String {
        BrowserOrigin::parse(text).unwrap().as_str().to_owned()
    }

    #[test]
    fn m7_origin_canonical_case_and_default_ports() {
        assert_eq!(site("https://example.com"), "https://example.com");
        assert_eq!(site("HTTPS://Example.COM"), "https://example.com");
        assert_eq!(site("https://example.com:443"), "https://example.com");
        assert_eq!(site("https://example.com:8443"), "https://example.com:8443");
        assert_eq!(site("http://localhost:80"), "http://localhost");
        assert_eq!(site("https://example.com:0443"), "https://example.com");
        // Equal sites are equal text, and only then.
        assert_eq!(
            BrowserOrigin::parse("https://EXAMPLE.com:443").unwrap(),
            BrowserOrigin::parse("https://example.com").unwrap()
        );
        assert_ne!(
            BrowserOrigin::parse("https://example.com:444").unwrap(),
            BrowserOrigin::parse("https://example.com").unwrap()
        );
        // No wildcard: a subdomain is another site.
        assert_ne!(
            BrowserOrigin::parse("https://www.example.com").unwrap(),
            BrowserOrigin::parse("https://example.com").unwrap()
        );
        assert_eq!(
            BrowserOrigin::parse("https://*.example.com"),
            Err(AddressError::Host)
        );
    }

    #[test]
    fn m7_origin_canonical_international_names_take_their_ascii_form() {
        assert_eq!(
            site("https://b\u{fc}cher.example"),
            "https://xn--bcher-kva.example"
        );
        assert_eq!(
            site("https://B\u{dc}CHER.example"),
            "https://xn--bcher-kva.example"
        );
        assert_eq!(
            site("https://xn--bcher-kva.example"),
            "https://xn--bcher-kva.example"
        );
        assert!(site("https://\u{4f8b}\u{3048}.example").is_ascii());
    }

    #[test]
    fn m7_origin_canonical_paths_queries_fragments_and_user_names_are_refused() {
        use AddressError as E;
        for (text, error) in [
            ("https://example.com/", E::Path),
            ("https://example.com/a", E::Path),
            ("https://example.com:443/", E::Path),
            ("https://example.com?", E::Query),
            ("https://example.com?q=1", E::Query),
            ("https://example.com#", E::Fragment),
            ("https://example.com#top", E::Fragment),
            ("https://user@example.com", E::UserName),
            ("https://user:secret@example.com", E::UserName),
            ("https://@example.com", E::UserName),
            ("https://:@example.com", E::UserName),
            ("https://example.com\\@other.example", E::Malformed),
            (" https://example.com", E::Malformed),
            ("https://example.com ", E::Malformed),
            ("https://exa\tmple.com", E::Malformed),
            ("https://example.com\n", E::Malformed),
            ("https://", E::Malformed),
            ("example.com", E::Malformed),
            ("", E::Malformed),
        ] {
            assert_eq!(BrowserOrigin::parse(text), Err(error), "{text:?}");
        }
    }

    #[test]
    fn m7_origin_canonical_other_schemes_and_plain_http() {
        use AddressError as E;
        for text in [
            "ftp://example.com",
            "file:///c:/users",
            "about:blank",
            "data:text/html,hello",
            "javascript:alert(1)",
            "ws://example.com",
            "wss://example.com",
            "blob:https://example.com/1",
        ] {
            assert_eq!(BrowserOrigin::parse(text), Err(E::Scheme), "{text:?}");
        }
        // `http` is for the fixture site only.
        assert_eq!(site("http://127.0.0.1:8080"), "http://127.0.0.1:8080");
        assert_eq!(site("http://LOCALHOST:8080"), "http://localhost:8080");
        for text in [
            "http://example.com",
            "http://127.0.0.2",
            "http://[::1]",
            "http://localhost.example",
            "http://0.0.0.0",
        ] {
            assert_eq!(BrowserOrigin::parse(text), Err(E::PlainHttp), "{text:?}");
        }
        // A number is read as the address it is, not as a name.
        assert_eq!(site("http://2130706433"), "http://127.0.0.1");
        assert_eq!(site("https://[::1]:8443"), "https://[::1]:8443");
    }

    #[test]
    fn m7_origin_canonical_the_267_byte_bound() {
        // Four labels: 63 + 1 + 63 + 1 + 63 + 1 + 61 = 253 bytes.
        let label = "a".repeat(63);
        let host = format!("{label}.{label}.{label}.{}", "a".repeat(61));
        assert_eq!(host.len(), 253);
        let longest = format!("https://{host}:65535");
        assert_eq!(longest.len(), MAX_ORIGIN_BYTES);
        assert_eq!(site(&longest), longest);
        let over = format!("https://{host}a:65535");
        assert_eq!(BrowserOrigin::parse(&over), Err(AddressError::TooLong));
        let far_over = format!("https://{}", "a.".repeat(2000));
        assert_eq!(BrowserOrigin::parse(&far_over), Err(AddressError::TooLong));
    }

    #[test]
    fn m7_origin_canonical_starting_addresses_are_kept_byte_for_byte() {
        for text in [
            "https://example.com/",
            "https://example.com",
            "HTTPS://Example.COM:443/Issues?state=Open&q=a%20b#Top",
            "https://example.com/a/../b/./c",
            "https://example.com/?",
            "http://127.0.0.1:41000/issues",
            "https://b\u{fc}cher.example/caf\u{e9}",
        ] {
            let address = StartingAddress::parse(text).unwrap();
            assert_eq!(address.as_str(), text);
            assert_eq!(address.as_str().as_bytes(), text.as_bytes());
        }
        let address = StartingAddress::parse("HTTPS://Example.COM:443/Issues?q=1").unwrap();
        assert_eq!(address.origin().as_str(), "https://example.com");

        use AddressError as E;
        for (text, error) in [
            ("http://example.com/issues", E::PlainHttp),
            ("ftp://example.com/issues", E::Scheme),
            ("https://user@example.com/issues", E::UserName),
            ("https://example.com/a b", E::Malformed),
            ("https://example.com/a\\b", E::Malformed),
            ("/issues", E::Malformed),
        ] {
            assert_eq!(StartingAddress::parse(text).err(), Some(error), "{text:?}");
        }
        // A user name after the authority is only text in a path.
        assert!(StartingAddress::parse("https://example.com/@user").is_ok());

        let fits = format!("https://example.com/{}", "a".repeat(2048 - 20));
        assert_eq!(fits.len(), MAX_STARTING_ADDRESS_BYTES);
        assert_eq!(StartingAddress::parse(&fits).unwrap().as_str(), fits);
        assert_eq!(
            StartingAddress::parse(&format!("{fits}a")).err(),
            Some(E::TooLong)
        );
    }

    #[test]
    fn m7_origin_canonical_a_bare_site_name_gets_its_root_address() {
        let address = StartingAddress::for_site_name("Example.COM").unwrap();
        assert_eq!(address.as_str(), "https://example.com/");
        assert_eq!(address.origin().as_str(), "https://example.com");
        // The address it makes is one `parse` accepts unchanged.
        assert_eq!(
            StartingAddress::parse(address.as_str()).unwrap().as_str(),
            address.as_str()
        );
        for name in [
            "",
            "example.com/a",
            "user@example.com",
            "https://example.com",
            "a b",
        ] {
            assert!(StartingAddress::for_site_name(name).is_err(), "{name:?}");
        }
    }

    #[test]
    fn m7_origin_canonical_reduction_keeps_the_site_and_nothing_else() {
        let reduced = AddressSite::of("https://Example.com:443/issues/1?token=MARK#frag");
        assert_eq!(reduced.record(), "https://example.com");
        // A user name is not part of a site.
        assert_eq!(
            AddressSite::of("https://user:secret@example.com/a").record(),
            "https://example.com"
        );
        assert_eq!(AddressSite::of("about:blank").record(), "blank");
        assert_eq!(
            AddressSite::of("http://example.com/a").record(),
            "plain_http"
        );
        for other in [
            "data:text/html,MARK",
            "blob:https://a.example/1",
            "file:///c:/x",
        ] {
            assert_eq!(AddressSite::of(other).record(), "other_scheme");
        }
        assert_eq!(AddressSite::of("not an address").record(), "unparsed");
        let long = format!("https://example.com/{}", "a".repeat(MAX_PAGE_ADDRESS_BYTES));
        assert_eq!(AddressSite::of(&long).record(), "unparsed");
        assert!(PageAddress::reported(&long).is_none());

        let page = PageAddress::reported("https://example.com/issues/1?token=MARK#frag").unwrap();
        assert_eq!(page.path(), "/issues/1");
        assert_eq!(page.site().record(), "https://example.com");
        assert_eq!(
            page.as_str(),
            "https://example.com/issues/1?token=MARK#frag"
        );
    }

    #[test]
    fn m7_origin_canonical_debug_shows_no_site_and_no_address() {
        let origin = BrowserOrigin::parse("https://secret-site.example").unwrap();
        let address = StartingAddress::parse("https://secret-site.example/p?token=MARK").unwrap();
        let page = PageAddress::from(&address);
        let reduced = AddressSite::of(address.as_str());
        let shown = format!("{origin:?} {address:?} {page:?} {reduced:?}");
        assert!(!shown.contains("secret-site"), "{shown}");
        assert!(!shown.contains("MARK"), "{shown}");
        assert!(!shown.contains("https"), "{shown}");
        // An error is a token.
        let error = BrowserOrigin::parse("https://secret-site.example/p").unwrap_err();
        assert_eq!(format!("{error:?}"), "Path");
    }
}
