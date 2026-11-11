//! Fetching GitHub's latest release with `NSURLSession`: an ephemeral session (no cookies,
//! no cache, no credentials), a plain GET with a generic user agent, a 20 s timeout. Nothing
//! about the user or the Mac is sent ([`request`]: the language header macOS would fill from
//! the user's preferred languages is fixed too). The answer is judged by [`crate::updates::outcome`]
//! and handed to the main thread.

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2_foundation::{
    NSData, NSError, NSHTTPURLResponse, NSMutableURLRequest, NSString, NSURL, NSURLRequestCachePolicy, NSURLResponse,
    NSURLSession, NSURLSessionConfiguration,
};

use crate::updates::{self, LATEST_URL, Outcome};

/// How long a check may take.
const TIMEOUT_SECS: f64 = 20.0;

/// Test builds only: the version the check offers instead of asking GitHub.
#[cfg(feature = "update-test")]
pub const TEST_VERSION_ENV: &str = "STAGE_MANAGER_UPDATE_VERSION";

/// Test builds only: a release of the version in [`TEST_VERSION_ENV`], when set.
#[cfg(feature = "update-test")]
fn test_release() -> Option<Outcome> {
    let text = std::env::var(TEST_VERSION_ENV).ok()?;
    let version = updates::Version::parse(&text)?;
    let tag = format!("v{version}");
    tracing::warn!(%version, "update check: test build, offering a local release");
    Some(Outcome::Available(updates::Release {
        dmg: Some(format!("{}{tag}/{}", updates::ASSET_PREFIX, updates::DMG_NAME)),
        page: updates::RELEASES_URL.to_string(),
        notes: "- A local test release".into(),
        dmg_size: None,
        version,
        tag,
    }))
}

/// The language every request names. Left alone, `NSURLSession` sends the user's own list of
/// preferred languages (`Accept-Language: de-DE,de;q=0.9,…`), which says something about
/// them; the answers are the same in any language.
const ACCEPT_LANGUAGE: &str = "en";

/// The request the check and the download send: a GET of `url` that reads no cache and
/// takes no cookies, with the app's name and version as its user agent, what it accepts,
/// and one fixed language ([`ACCEPT_LANGUAGE`]): nothing in it comes from the user's Mac.
pub fn request(url: &NSURL, accept: &str, timeout_secs: f64) -> Retained<NSMutableURLRequest> {
    let request = NSMutableURLRequest::requestWithURL_cachePolicy_timeoutInterval(
        url,
        NSURLRequestCachePolicy::ReloadIgnoringLocalCacheData,
        timeout_secs,
    );
    request.setValue_forHTTPHeaderField(Some(&NSString::from_str(accept)), &NSString::from_str("Accept"));
    request.setValue_forHTTPHeaderField(
        Some(&NSString::from_str(&format!("Stage-Manager/{}", env!("CARGO_PKG_VERSION")))),
        &NSString::from_str("User-Agent"),
    );
    request.setValue_forHTTPHeaderField(Some(&NSString::from_str(ACCEPT_LANGUAGE)), &NSString::from_str("Accept-Language"));
    request.setHTTPShouldHandleCookies(false);
    request
}

/// Starts a check; `done` runs on the main thread with the outcome.
pub fn check(done: impl FnOnce(Outcome) + Send + 'static) {
    #[cfg(feature = "update-test")]
    if let Some(outcome) = test_release() {
        DispatchQueue::main().exec_async(move || done(outcome));
        return;
    }
    let Some(url) = NSURL::URLWithString(&NSString::from_str(LATEST_URL)) else {
        done(Outcome::Failed("bad URL".into()));
        return;
    };
    let request = request(&url, "application/vnd.github+json", TIMEOUT_SECS);
    let config = NSURLSessionConfiguration::ephemeralSessionConfiguration();
    let session = NSURLSession::sessionWithConfiguration(&config);
    let done = std::sync::Mutex::new(Some(done));
    let handler = RcBlock::new(move |data: *mut NSData, response: *mut NSURLResponse, error: *mut NSError| {
        // SAFETY: NSURLSession passes valid (possibly nil) objects for the block's duration.
        let (data, response, error) = unsafe { (data.as_ref(), response.as_ref(), error.as_ref()) };
        let outcome = match (error, response) {
            (Some(e), _) => Outcome::Failed(e.localizedDescription().to_string()),
            (None, Some(r)) => {
                let status = r.downcast_ref::<NSHTTPURLResponse>().map_or(0, |h| h.statusCode() as i64);
                let body = data.map(|d| String::from_utf8_lossy(&d.to_vec()).into_owned()).unwrap_or_default();
                updates::outcome(&updates::current(), status, &body)
            }
            (None, None) => Outcome::Failed("no answer".into()),
        };
        let Some(done) = done.lock().ok().and_then(|mut d| d.take()) else { return };
        DispatchQueue::main().exec_async(move || done(outcome));
    });
    // SAFETY: a valid request and a completion block that lives as long as the task needs it.
    let task = unsafe { session.dataTaskWithRequest_completionHandler(&request, &handler) };
    task.resume();
    session.finishTasksAndInvalidate();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};
    use std::time::Duration;

    /// What a request built by [`request`] puts on the wire, read by a listener on this Mac:
    /// the request itself, and the one that follows the redirect it is answered with (a
    /// release's download is redirected to another host).
    fn headers_sent() -> [String; 2] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a local port");
        let port = listener.local_addr().expect("address").port();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for answer in [
                "HTTP/1.1 302 Found\r\nLocation: /asset\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
            ] {
                let Ok((mut stream, _)) = listener.accept() else { return };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let (mut seen, mut buf) = (Vec::new(), [0u8; 1024]);
                while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf) {
                        Ok(n) if n > 0 => seen.extend_from_slice(&buf[..n]),
                        _ => break,
                    }
                }
                let _ = stream.write_all(answer.as_bytes());
                let _ = tx.send(String::from_utf8_lossy(&seen).into_owned());
            }
        });
        let url = NSURL::URLWithString(&NSString::from_str(&format!("http://127.0.0.1:{port}/latest"))).expect("url");
        let request = request(&url, "application/vnd.github+json", 5.0);
        let session = NSURLSession::sessionWithConfiguration(&NSURLSessionConfiguration::ephemeralSessionConfiguration());
        let handler = RcBlock::new(|_: *mut NSData, _: *mut NSURLResponse, _: *mut NSError| {});
        // SAFETY: a valid request and a completion block that lives as long as the task needs it.
        let task = unsafe { session.dataTaskWithRequest_completionHandler(&request, &handler) };
        task.resume();
        session.finishTasksAndInvalidate();
        let wait = Duration::from_secs(10);
        [rx.recv_timeout(wait).expect("the request arrived"), rx.recv_timeout(wait).expect("the redirect was followed")]
    }

    /// "Nothing about you or this Mac is sent": the request names the app and its version,
    /// and no language of the user's (macOS adds their preferred languages by itself).
    #[test]
    fn a_request_carries_nothing_of_the_users() {
        for sent in headers_sent() {
            let headers: Vec<(String, &str)> =
                sent.lines().skip(1).filter_map(|l| l.split_once(": ")).map(|(k, v)| (k.to_ascii_lowercase(), v.trim())).collect();
            let value = |name: &str| headers.iter().find(|(k, _)| k == name).map(|(_, v)| *v);
            assert_eq!(value("accept-language"), Some("en"), "{sent}");
            assert_eq!(value("user-agent"), Some(format!("Stage-Manager/{}", env!("CARGO_PKG_VERSION")).as_str()), "{sent}");
            assert_eq!(value("accept"), Some("application/vnd.github+json"), "{sent}");
            // The rest is the connection's own business; nothing else rides along (no cookie).
            let known = ["host", "accept", "accept-language", "user-agent", "accept-encoding", "connection"];
            let others: Vec<&String> = headers.iter().map(|(k, _)| k).filter(|k| !known.contains(&k.as_str())).collect();
            assert!(others.is_empty(), "{sent}");
        }
    }
}
