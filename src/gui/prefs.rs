//! What the app remembers between launches, kept in the user defaults.

use std::collections::{BTreeMap, BTreeSet};

use objc2::rc::Retained;
use objc2_foundation::{NSArray, NSString, NSUserDefaults};
use usbip_macos::app::DeviceKey;

const SERVERS: &str = "servers";
/// Single server from before the app could hold several.
const LEGACY_SERVER: &str = "server";
const ATTACHED: &str = "attached";
/// Product names learned from attached devices, as `vvvv:pppp=name`.
const NAMES: &str = "names";

fn defaults() -> Retained<NSUserDefaults> {
    NSUserDefaults::standardUserDefaults()
}

fn strings(key: &str) -> Vec<String> {
    defaults()
        .stringArrayForKey(&NSString::from_str(key))
        .map(|a| a.iter().map(|s| s.to_string()).collect())
        .unwrap_or_default()
}

fn set_strings<'a>(key: &str, values: impl IntoIterator<Item = &'a str>) {
    let values: Vec<Retained<NSString>> = values.into_iter().map(NSString::from_str).collect();
    let array = NSArray::from_retained_slice(&values);
    unsafe { defaults().setObject_forKey(Some(&array), &NSString::from_str(key)) };
}

pub fn servers() -> Vec<String> {
    let servers = strings(SERVERS);
    if !servers.is_empty() {
        return servers;
    }
    defaults()
        .stringForKey(&NSString::from_str(LEGACY_SERVER))
        .map(|s| vec![s.to_string()])
        .unwrap_or_default()
}

pub fn save_servers(servers: &[String]) {
    set_strings(SERVERS, servers.iter().map(String::as_str));
    defaults().removeObjectForKey(&NSString::from_str(LEGACY_SERVER));
}

pub fn attached() -> BTreeSet<DeviceKey> {
    strings(ATTACHED)
        .iter()
        .filter_map(|s| DeviceKey::decode(s))
        .collect()
}

pub fn save_attached(keys: &BTreeSet<DeviceKey>) {
    let encoded: Vec<String> = keys.iter().map(DeviceKey::encode).collect();
    set_strings(ATTACHED, encoded.iter().map(String::as_str));
}

pub fn names() -> BTreeMap<(u16, u16), String> {
    strings(NAMES)
        .iter()
        .filter_map(|entry| {
            let (ids, name) = entry.split_once('=')?;
            let (vid, pid) = ids.split_once(':')?;
            let vid = u16::from_str_radix(vid, 16).ok()?;
            let pid = u16::from_str_radix(pid, 16).ok()?;
            Some(((vid, pid), name.to_string()))
        })
        .collect()
}

pub fn save_names(names: &BTreeMap<(u16, u16), String>) {
    let encoded: Vec<String> = names
        .iter()
        .map(|((vid, pid), name)| format!("{vid:04x}:{pid:04x}={name}"))
        .collect();
    set_strings(NAMES, encoded.iter().map(String::as_str));
}
