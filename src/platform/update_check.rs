//! Fetching GitHub's latest release with `NSURLSession`: an ephemeral session (no cookies,
//! no cache, no credentials), a plain GET with a generic user agent, a 20 s timeout. Nothing
//! about the user or the Mac is sent. The answer is judged by [`crate::updates::outcome`]
//! and handed to the main thread.

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2_foundation::{
    NSData, NSError, NSHTTPURLResponse, NSMutableURLRequest, NSString, NSURL, NSURLRequestCachePolicy, NSURLResponse,
    NSURLSession, NSURLSessionConfiguration,
};

use crate::updates::{self, LATEST_URL, Outcome};

/// How long a check may take.
const TIMEOUT_SECS: f64 = 20.0;

/// Starts a check; `done` runs on the main thread with the outcome.
pub fn check(done: impl FnOnce(Outcome) + Send + 'static) {
    let Some(url) = NSURL::URLWithString(&NSString::from_str(LATEST_URL)) else {
        done(Outcome::Failed("bad URL".into()));
        return;
    };
    let request = NSMutableURLRequest::requestWithURL_cachePolicy_timeoutInterval(
        &url,
        NSURLRequestCachePolicy::ReloadIgnoringLocalCacheData,
        TIMEOUT_SECS,
    );
    request.setValue_forHTTPHeaderField(Some(&NSString::from_str("application/vnd.github+json")), &NSString::from_str("Accept"));
    request.setValue_forHTTPHeaderField(
        Some(&NSString::from_str(&format!("Stage-Manager/{}", env!("CARGO_PKG_VERSION")))),
        &NSString::from_str("User-Agent"),
    );
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
