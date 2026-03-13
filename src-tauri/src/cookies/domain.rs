use std::collections::HashMap;

/// Normalize a cookie domain: lowercase and strip leading dot.
pub fn normalize_cookie_domain(domain: &str) -> String {
    domain.to_lowercase().trim_start_matches('.').to_string()
}

/// Check if a domain belongs to google.com.
pub fn is_google_domain(domain: &str) -> bool {
    let norm = normalize_cookie_domain(domain);
    norm == "google.com" || norm.ends_with(".google.com")
}

/// Priority for domain selection: .google.com=0, *.google.com=1, other=9.
fn cookie_domain_priority(domain: &str) -> u8 {
    let norm = normalize_cookie_domain(domain);
    if norm == "google.com" {
        0
    } else if norm.ends_with(".google.com") {
        1
    } else {
        9
    }
}

pub struct CookieItem {
    pub name: String,
    pub value: String,
    pub domain: String,
}

/// Select preferred Google cookies by domain priority (matching Python logic).
pub fn select_preferred_google_cookies(items: &[CookieItem]) -> HashMap<String, String> {
    let mut selected: HashMap<String, String> = HashMap::new();
    let mut selected_priority: HashMap<String, u8> = HashMap::new();

    for item in items {
        if item.name.is_empty() || !is_google_domain(&item.domain) {
            continue;
        }

        let prio = cookie_domain_priority(&item.domain);
        let prev_prio = selected_priority.get(&item.name).copied();

        if prev_prio.is_none() || prio < prev_prio.unwrap() {
            selected.insert(item.name.clone(), item.value.clone());
            selected_priority.insert(item.name.clone(), prio);
        }
    }

    selected
}
