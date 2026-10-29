//! Looks for a newer release on GitHub.

use block2::RcBlock;
use dispatch2::DispatchQueue;
use log::{debug, info};
use objc2_foundation::{
    ns_string, NSData, NSDictionary, NSError, NSJSONReadingOptions, NSJSONSerialization, NSString,
    NSURLResponse, NSURLSession, NSURL,
};
use usbip_macos::app::is_newer;

const LATEST_RELEASE: &str = "https://api.github.com/repos/karstx/usbip-macos/releases/latest";

pub struct Release {
    pub version: String,
    pub url: String,
}

fn string(dict: &NSDictionary, key: &NSString) -> Option<String> {
    Some(
        dict.objectForKey(key)?
            .downcast::<NSString>()
            .ok()?
            .to_string(),
    )
}

fn parse(data: &NSData) -> Option<Release> {
    let json =
        NSJSONSerialization::JSONObjectWithData_options_error(data, NSJSONReadingOptions::empty())
            .ok()?;
    let release = json.downcast_ref::<NSDictionary>()?;
    Some(Release {
        version: string(release, ns_string!("tag_name"))?,
        url: string(release, ns_string!("html_url"))?,
    })
}

/// Calls `found` on the main queue if the latest release is newer than this build.
pub fn check(found: impl Fn(Release) + Send + Sync + 'static) {
    let Some(url) = NSURL::URLWithString(&NSString::from_str(LATEST_RELEASE)) else {
        return;
    };
    let found = std::sync::Arc::new(found);
    let done = RcBlock::new(
        move |data: *mut NSData, _: *mut NSURLResponse, error: *mut NSError| {
            if let Some(e) = unsafe { error.as_ref() } {
                debug!("update check failed: {}", e.localizedDescription());
                return;
            }
            let Some(release) = unsafe { data.as_ref() }.and_then(parse) else {
                return;
            };
            if !is_newer(&release.version, env!("CARGO_PKG_VERSION")) {
                return;
            }
            info!("{} is available", release.version);
            let found = std::sync::Arc::clone(&found);
            DispatchQueue::main().exec_async(move || found(release));
        },
    );
    let task =
        unsafe { NSURLSession::sharedSession().dataTaskWithURL_completionHandler(&url, &done) };
    task.resume();
}
