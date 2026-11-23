//! A scripted Google for unit tests: replies are matched on a URL fragment
//! (first match wins, each reply is used once if `once`) and every request is
//! recorded.

use crate::cloud::{CloudHttpRequest, CloudHttpResponse, CloudHttpTransport, QuarkError};
use std::sync::Mutex;

pub struct Rule {
    pub needle: &'static str,
    pub replies: Vec<(u16, String)>,
}

pub struct ScriptedGoogle {
    rules: Mutex<Vec<Rule>>,
    pub requests: Mutex<Vec<CloudHttpRequest>>,
}

impl ScriptedGoogle {
    pub fn new(rules: Vec<Rule>) -> Self {
        Self {
            rules: Mutex::new(rules),
            requests: Mutex::new(vec![]),
        }
    }

    pub fn requests_to(&self, needle: &str) -> Vec<CloudHttpRequest> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.url.contains(needle))
            .cloned()
            .collect()
    }
}

pub fn rule(needle: &'static str, replies: Vec<(u16, &str)>) -> Rule {
    Rule {
        needle,
        replies: replies
            .into_iter()
            .map(|(status, body)| (status, body.to_owned()))
            .collect(),
    }
}

impl CloudHttpTransport for ScriptedGoogle {
    fn send(&self, request: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
        self.requests.lock().unwrap().push(request.clone());
        let mut rules = self.rules.lock().unwrap();
        let rule = rules
            .iter_mut()
            .find(|rule| request.url.contains(rule.needle))
            .unwrap_or_else(|| panic!("unscripted request {}", request.url));
        // The last reply repeats, earlier ones are consumed in order.
        let (status, body) = if rule.replies.len() > 1 {
            rule.replies.remove(0)
        } else {
            rule.replies[0].clone()
        };
        Ok(CloudHttpResponse {
            status,
            body,
            set_cookies: vec![],
        })
    }
}
