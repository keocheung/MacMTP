use objc2::rc::Retained;
use objc2_foundation::{NSBundle, NSString};

pub fn tr(key: &str) -> String {
    let key = NSString::from_str(key);
    NSBundle::mainBundle()
        .localizedStringForKey_value_table(&key, None, None)
        .to_string()
}

pub fn ns_tr(key: &str) -> Retained<NSString> {
    NSString::from_str(&tr(key))
}
