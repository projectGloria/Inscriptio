//! Minimal URL splitter. We only *rewrite* links we already trust enough to
//! have been copied, so we skip validation, IDNA and percent-decoding. That's
//! what keeps the `url` crate (and its ~200 KB of Unicode tables) out of the binary.

/// Borrowed view into an `http(s)` URL. No allocation: every field is a slice of the input.
#[derive(Debug, Clone, Copy)]
pub struct Url<'a> {
    pub scheme: &'a str,   // "http" | "https" (original casing)
    pub host: &'a str,     // host[:port], without userinfo
    pub path: &'a str,     // "" or starts with '/'
    pub query: &'a str,    // without leading '?'
    pub fragment: &'a str, // without leading '#'
}

impl<'a> Url<'a> {
    pub fn parse(s: &'a str) -> Option<Self> {
        let (scheme, rest) = s.split_once("://")?;
        if !(scheme.eq_ignore_ascii_case("https") || scheme.eq_ignore_ascii_case("http")) {
            return None;
        }

        // The authority ends at the first '/', '?' or '#'. Finding by bytes is
        // safe because all three are ASCII, so the index is always a char boundary.
        let auth_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let (host, rest) = rest.split_at(auth_end);
        // A URL with userinfo (`user:pass@host`) is either a phishing trick or
        // something we shouldn't touch. Bail out.
        if host.is_empty() || host.contains('@') {
            return None;
        }

        let (rest, fragment) = rest.split_once('#').unwrap_or((rest, ""));
        let (path, query) = rest.split_once('?').unwrap_or((rest, ""));

        Some(Url { scheme, host, path, query, fragment })
    }

    /// Non-empty `key=value` pairs, verbatim (still percent-encoded).
    pub fn params(&self) -> impl Iterator<Item = &'a str> + Clone + 'a {
        self.query.split('&').filter(|p| !p.is_empty())
    }

    /// Value of the first `key=` param (raw), if any.
    pub fn param(&self, key: &str) -> Option<&'a str> {
        self.params().find_map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (k == key).then_some(v)
        })
    }

    /// Non-empty path segments: "/a//b/" → ["a", "b"].
    pub fn segments(&self) -> impl Iterator<Item = &'a str> + 'a {
        self.path.split('/').filter(|s| !s.is_empty())
    }
}

/// Key part of a `key=value` pair.
#[inline]
pub fn key(pair: &str) -> &str {
    pair.split_once('=').map_or(pair, |(k, _)| k)
}

/// Builds `scheme://host path [?p1&p2…] [#frag]` with a single allocation
/// sized up front, so the String never reallocates while we push.
pub fn assemble<'a>(
    scheme: &str,
    host: &str,
    path: &str,
    params: impl Iterator<Item = &'a str> + Clone,
    fragment: &str,
) -> String {
    let cap = scheme.len() + 3 + host.len() + path.len() + fragment.len() + 2
        + params.clone().map(|p| p.len() + 1).sum::<usize>();
    let mut out = String::with_capacity(cap);
    out.push_str(scheme);
    out.push_str("://");
    out.push_str(host);
    out.push_str(path);
    for (i, p) in params.enumerate() {
        out.push(if i == 0 { '?' } else { '&' });
        out.push_str(p);
    }
    if !fragment.is_empty() {
        out.push('#');
        out.push_str(fragment);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_all_parts() {
        let u = Url::parse("https://Ex.com:8080/a/b?x=1&y#frag?no").unwrap();
        assert_eq!(u.scheme, "https");
        assert_eq!(u.host, "Ex.com:8080");
        assert_eq!(u.path, "/a/b");
        assert_eq!(u.query, "x=1&y");
        assert_eq!(u.fragment, "frag?no"); // '?' after '#' belongs to the fragment
        assert_eq!(u.param("x"), Some("1"));
        assert_eq!(u.param("y"), Some(""));
    }

    #[test]
    fn host_only_and_rejects() {
        let u = Url::parse("https://ex.com?q").unwrap();
        assert_eq!((u.host, u.path, u.query), ("ex.com", "", "q"));
        assert!(Url::parse("mailto:a@b.c").is_none());
        assert!(Url::parse("ftp://ex.com").is_none());
        assert!(Url::parse("https://user:pw@ex.com/").is_none());
    }
}
