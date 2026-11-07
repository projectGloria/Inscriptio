//! Pure link-shortening rules. No Win32 code, so all of it runs under `cargo test`.
//!
//! Pipeline: host-specific canonical rewrite (if one matches) → otherwise a
//! generic strip of tracking params, plus any per-host junk params.

use crate::url::{Url, assemble, key};

/// Anything longer is a blob, not a link someone means to share.
const MAX_LEN: usize = 4096;

/// User-facing knobs. `Copy` + passed by value: two bools fit in a register,
/// and passing it keeps `shorten` pure (no hidden global reads), so tests stay deterministic.
#[derive(Clone, Copy)]
pub struct Opts {
    /// Keep YouTube `t=` (start time). Off = always link to the start of the video.
    pub keep_time: bool,
}

impl Default for Opts {
    fn default() -> Self {
        Opts { keep_time: true }
    }
}

/// Returns `Some(shorter)` only when the link actually changes. Returning
/// `None` lets the caller skip the clipboard write, which avoids firing a
/// pointless WM_CLIPBOARDUPDATE at every other listener on the system.
pub fn shorten(input: &str, o: Opts) -> Option<String> {
    let s = input.trim();
    // Any internal whitespace means prose, or several links. Not ours to touch.
    if s.is_empty() || s.len() > MAX_LEN || s.bytes().any(|b| b.is_ascii_whitespace()) {
        return None;
    }
    let u = Url::parse(s)?;
    let out = rewrite(&u, o);
    (out != s).then_some(out)
}

fn rewrite(u: &Url, o: Opts) -> String {
    // One small allocation for case-insensitive host matching. The generic path
    // still emits the *original* host, so we never change casing we don't own.
    let host = u.host.to_ascii_lowercase();
    let h = host.split(':').next().unwrap_or(&host); // drop :port for matching
    let bare = h.strip_prefix("www.").or_else(|| h.strip_prefix("m.")).unwrap_or(h);

    // Each arm either returns a canonical rewrite or picks extra junk keys for
    // the generic fallback. `match` on &str compiles to length + memcmp checks,
    // which is cheaper than any runtime rule table.
    let extra: &[&str] = match bare {
        "youtube.com" => {
            if let Some(s) = youtube(u, o) {
                return s;
            }
            YT_JUNK
        }
        "youtu.be" => return youtu_be(u, o),
        "music.youtube.com" => YT_JUNK, // keep the Music host: youtu.be would open the video player
        "x.com" | "twitter.com" | "mobile.twitter.com" | "mobile.x.com" => return twitter(u),
        "open.spotify.com" => &["si", "context", "nd"],
        "reddit.com" | "old.reddit.com" | "new.reddit.com" => {
            if let Some(s) = reddit(u) {
                return s;
            }
            &["share_id", "ref", "ref_source", "rdt"]
        }
        b if b.starts_with("amazon.") => {
            if let Some(s) = amazon(u, b) {
                return s;
            }
            &["ref", "ref_", "pd_rd_w", "pd_rd_r", "pd_rd_wg", "pf_rd_p", "pf_rd_r", "content-id", "psc"]
        }
        b if b.starts_with("ebay.") => {
            if let Some(s) = ebay(u, b) {
                return s;
            }
            &["_trkparms", "_trksid", "hash", "amdata", "mkevt", "mkcid", "mkrid", "campid"]
        }
        b if b.starts_with("aliexpress.") && u.path.starts_with("/item/") => {
            // The item ID lives in the path. Every query param is tracking or A/B noise.
            return assemble(u.scheme, u.host, u.path, std::iter::empty(), "");
        }
        b if b.starts_with("google.") && u.path == "/search" => {
            // Keep the query and the result-type switches (images, `udm=14` web-only).
            return assemble(
                u.scheme,
                u.host,
                u.path,
                u.params().filter(|p| matches!(key(p), "q" | "tbm" | "udm")),
                "",
            );
        }
        _ => &[],
    };
    generic(u, extra)
}

const YT_JUNK: &[&str] = &["si", "pp", "feature", "ab_channel", "embeds_referring_euri", "source_ve_path"];
/// YouTube params that change *what* plays, so they must survive.
fn yt_keep(k: &str, o: Opts) -> bool {
    matches!(k, "list" | "index") || (o.keep_time && k == "t")
}

fn generic(u: &Url, extra: &[&str]) -> String {
    assemble(
        u.scheme,
        u.host,
        u.path,
        u.params().filter(|p| {
            let k = key(p);
            !is_tracker(k) && !extra.contains(&k)
        }),
        u.fragment,
    )
}

/// Cross-site tracking params. These never change the page content.
fn is_tracker(k: &str) -> bool {
    let b = k.as_bytes();
    // Byte-slice compare: `k[..4]` could panic on a multibyte char boundary.
    (b.len() > 4 && b[..4].eq_ignore_ascii_case(b"utm_"))
        || matches!(
            k,
            "fbclid" | "gclid" | "dclid" | "gbraid" | "wbraid" | "msclkid" | "yclid" | "twclid"
                | "ttclid" | "li_fat_id" | "mc_cid" | "mc_eid" | "_hsenc" | "_hsmktg" | "igshid"
                | "igsh" | "ref_src" | "ref_url" | "spm" | "scm" | "_ga" | "_gl" | "oly_anon_id"
                | "oly_enc_id" | "vero_id" | "mkt_tok"
        )
}

// ---------------------------------------------------------------- YouTube

fn is_yt_id(id: &str) -> bool {
    id.len() == 11 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn youtube(u: &Url, o: Opts) -> Option<String> {
    let mut segs = u.segments();
    let id = match (segs.next(), segs.next(), segs.next()) {
        (Some("watch"), None, None) => u.param("v")?,
        (Some("shorts" | "live" | "embed" | "v"), Some(id), None) => id,
        _ => return None, // channels, playlists, /results… fall through to generic
    };
    is_yt_id(id).then(|| yt_short(id, u, o))
}

fn youtu_be(u: &Url, o: Opts) -> String {
    let mut segs = u.segments();
    match (segs.next(), segs.next()) {
        (Some(id), None) if is_yt_id(id) => yt_short(id, u, o),
        _ => generic(u, YT_JUNK),
    }
}

fn yt_short(id: &str, u: &Url, o: Opts) -> String {
    let path = format!("/{id}");
    assemble("https", "youtu.be", &path, u.params().filter(|p| yt_keep(key(p), o)), u.fragment)
}

// ---------------------------------------------------------------- Others

fn twitter(u: &Url) -> String {
    // x.com is 6 chars shorter than twitter.com, and twitter.com 301s there anyway.
    // `s` and `t` are share-sheet attribution, not content.
    assemble(
        "https",
        "x.com",
        u.path,
        u.params().filter(|p| {
            let k = key(p);
            !is_tracker(k) && k != "s" && k != "t"
        }),
        u.fragment,
    )
}

fn reddit(u: &Url) -> Option<String> {
    let mut segs = u.segments();
    let mut first = segs.next()?;
    if first == "r" {
        segs.next()?; // subreddit name: redd.it doesn't need it
        first = segs.next()?;
    }
    if first != "comments" {
        return None;
    }
    let id = segs.next()?;
    let _slug = segs.next();
    // A 6th segment is a comment ID. redd.it can't address comments, so keep
    // the full permalink and let the generic strip clean it.
    if segs.next().is_some() {
        return None;
    }
    let ok = (4..=10).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    ok.then(|| format!("https://redd.it/{id}"))
}

fn amazon(u: &Url, bare: &str) -> Option<String> {
    // ASINs show up as /dp/X, /gp/product/X, /gp/aw/d/X (mobile), always 10 uppercase alnum.
    let is_asin = |s: &str| s.len() == 10 && s.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    let mut prev = "";
    for seg in u.segments() {
        if matches!(prev, "dp" | "product" | "d") && is_asin(seg) {
            return Some(format!("https://{bare}/dp/{seg}"));
        }
        prev = seg;
    }
    None
}

fn ebay(u: &Url, bare: &str) -> Option<String> {
    let mut segs = u.segments();
    if segs.next()? != "itm" {
        return None;
    }
    // /itm/<slug>/<id> or /itm/<id>: the numeric item ID is always last.
    let id = segs.last()?;
    let ok = id.len() >= 9 && id.bytes().all(|b| b.is_ascii_digit());
    ok.then(|| format!("https://{bare}/itm/{id}"))
}

// ---------------------------------------------------------------- history guard

/// True when a link looks like it carries a credential. Such links are still
/// trimmed, but never written to history. False positives (a Google Docs ID
/// looks like a token) are the cheap side of this trade-off.
pub fn is_sensitive(link: &str) -> bool {
    let Some(u) = Url::parse(link) else { return true }; // unparseable: don't persist
    let secret_key = |k: &str| {
        let k = k.to_ascii_lowercase();
        matches!(
            k.as_str(),
            "code" | "state" | "key" | "auth" | "sig" | "sid" | "otp" | "pin" | "pass" | "pwd" | "jwt"
                | "nonce" | "ticket" | "hash"
        ) || ["token", "secret", "password", "session", "apikey", "api_key", "auth_", "signature", "credential"].iter().any(|n| k.contains(n))
    };
    // OAuth implicit flow puts tokens in the fragment: #access_token=…
    let frag_secret = u.fragment.split('&').any(|p| secret_key(key(p)));
    // Reset/magic/invite links embed the token in the path. Real content IDs are
    // short (YouTube 11, ASIN 10, Spotify 22); ≥32 mixed alnum chars smells like a secret.
    let path_token = u.segments().any(|s| {
        s.len() >= 32
            && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            && s.bytes().any(|b| b.is_ascii_digit())
            && s.bytes().any(|b| b.is_ascii_alphabetic())
    });
    frag_secret || path_token || u.params().any(|p| secret_key(key(p)))
}

#[cfg(test)]
mod tests {
    use super::{Opts, is_sensitive};

    fn shorten(s: &str) -> Option<String> {
        super::shorten(s, Opts::default())
    }

    #[track_caller]
    fn eq(input: &str, want: &str) {
        assert_eq!(shorten(input).as_deref(), Some(want), "input: {input}");
    }
    #[track_caller]
    fn none(input: &str) {
        assert_eq!(shorten(input), None, "input: {input}");
    }

    #[test]
    fn youtube() {
        eq("https://youtu.be/jdkJ3kjp6Ig?si=uzzTs2I6NxtdTDtK", "https://youtu.be/jdkJ3kjp6Ig");
        eq("https://www.youtube.com/watch?v=jdkJ3kjp6Ig", "https://youtu.be/jdkJ3kjp6Ig");
        eq("https://m.youtube.com/watch?v=jdkJ3kjp6Ig&pp=abc&t=42s", "https://youtu.be/jdkJ3kjp6Ig?t=42s");
        eq(
            "https://www.youtube.com/watch?v=jdkJ3kjp6Ig&list=PL123&index=3&si=x",
            "https://youtu.be/jdkJ3kjp6Ig?list=PL123&index=3",
        );
        eq("https://youtube.com/shorts/jdkJ3kjp6Ig?feature=share", "https://youtu.be/jdkJ3kjp6Ig");
        eq("https://www.youtube.com/live/jdkJ3kjp6Ig?si=abc", "https://youtu.be/jdkJ3kjp6Ig");
        eq("https://youtu.be/jdkJ3kjp6Ig?t=10&si=q", "https://youtu.be/jdkJ3kjp6Ig?t=10");
        eq("https://music.youtube.com/watch?v=jdkJ3kjp6Ig&si=x", "https://music.youtube.com/watch?v=jdkJ3kjp6Ig");
        eq(
            "https://www.youtube.com/@channel/videos?si=x",
            "https://www.youtube.com/@channel/videos",
        );
        none("https://youtu.be/jdkJ3kjp6Ig");
        none("https://www.youtube.com/watch?v=tooShort"); // invalid ID: leave alone
    }

    #[test]
    fn shops() {
        eq(
            "https://www.amazon.com/Some-Product-Name/dp/B0C1234567/ref=sr_1_3?crid=X&keywords=y",
            "https://amazon.com/dp/B0C1234567",
        );
        eq("https://www.amazon.co.uk/gp/product/B0C1234567?psc=1", "https://amazon.co.uk/dp/B0C1234567");
        eq(
            "https://www.ebay.com/itm/cool-thing-name/123456789012?_trkparms=x&hash=y",
            "https://ebay.com/itm/123456789012",
        );
        eq("https://www.aliexpress.com/item/1005001.html?spm=a2g0o&gatewayAdapt=x", "https://www.aliexpress.com/item/1005001.html");
    }

    #[test]
    fn social() {
        eq(
            "https://www.reddit.com/r/rust/comments/1abcde/some_title/?utm_source=share&share_id=x",
            "https://redd.it/1abcde",
        );
        eq(
            "https://www.reddit.com/r/rust/comments/1abcde/t/k9xyz12/?context=3&share_id=x",
            "https://www.reddit.com/r/rust/comments/1abcde/t/k9xyz12/?context=3",
        );
        eq("https://twitter.com/user/status/1234567890?s=20&t=abc", "https://x.com/user/status/1234567890");
        eq("https://open.spotify.com/track/abc123?si=zzz", "https://open.spotify.com/track/abc123");
        eq("https://www.instagram.com/p/Cxyz/?igsh=abc", "https://www.instagram.com/p/Cxyz/");
    }

    #[test]
    fn generic_and_search() {
        eq(
            "https://example.com/a?id=5&utm_source=x&UTM_Medium=y&fbclid=z#sec",
            "https://example.com/a?id=5#sec",
        );
        eq("https://www.google.com/search?q=rust&sca_esv=1&ei=2&udm=14", "https://www.google.com/search?q=rust&udm=14");
        eq("  https://example.com/?gclid=1  ", "https://example.com/");
    }

    #[test]
    fn timestamp_toggle() {
        let off = Opts { keep_time: false };
        assert_eq!(
            super::shorten("https://www.youtube.com/watch?v=jdkJ3kjp6Ig&t=2s&list=PL1", off).as_deref(),
            Some("https://youtu.be/jdkJ3kjp6Ig?list=PL1")
        );
        assert_eq!(super::shorten("https://youtu.be/jdkJ3kjp6Ig?t=2s", off).as_deref(), Some("https://youtu.be/jdkJ3kjp6Ig"));
    }

    #[test]
    fn sensitive_links() {
        assert!(is_sensitive("https://app.example.com/callback?code=abc&state=xyz"));
        assert!(is_sensitive("https://example.com/cb#access_token=abc&expires_in=3600"));
        assert!(is_sensitive("https://example.com/reset/9f8e7d6c5b4a39281706f5e4d3c2b1a0aa"));
        assert!(is_sensitive("https://bucket.s3.amazonaws.com/f?X-Amz-Signature=abc"));
        assert!(is_sensitive("https://x.com/?api_key=1"));
        assert!(!is_sensitive("https://youtu.be/jdkJ3kjp6Ig?t=2s"));
        assert!(!is_sensitive("https://open.spotify.com/track/1XD1Acc4XiKaScHEg0M3Av"));
        assert!(!is_sensitive("https://amazon.com/dp/B0C1234567"));
    }

    #[test]
    fn leaves_non_links_alone() {
        none("hello world");
        none("https://example.com/a https://example.com/b");
        none("https://example.com/a\r\nmore text");
        none("mailto:me@example.com");
        none("https://example.com/page?id=5");
        none("  https://example.com/page  "); // only whitespace differs: don't churn the clipboard
        none("");
    }
}
