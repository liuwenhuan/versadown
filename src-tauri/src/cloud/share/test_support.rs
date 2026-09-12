use super::*;
use crate::cloud::{CloudHttpRequest, CloudHttpResponse, CloudHttpTransport};
use serde_json::Value;
pub struct Http {
    pub requests: Mutex<Vec<CloudHttpRequest>>,
    replies: Mutex<std::collections::VecDeque<Value>>,
}
impl Http {
    pub fn new(replies: Vec<Value>) -> Self {
        Self {
            requests: Mutex::new(vec![]),
            replies: Mutex::new(replies.into()),
        }
    }
}
impl CloudHttpTransport for Http {
    fn send(&self, request: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
        self.requests.lock().unwrap().push(request);
        Ok(CloudHttpResponse {
            status: 200,
            body: self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected share HTTP request")
                .to_string(),
            set_cookies: vec!["SESSION=issued; Path=/; Secure".into()],
        })
    }
}
pub fn candidate(provider: CloudProvider) -> crate::parser::CloudShareCandidate {
    let url = match provider {
        CloudProvider::Aliyun => "https://www.alipan.com/s/share123",
        CloudProvider::Pikpak => "https://mypikpak.com/s/share123",
        CloudProvider::Tianyi => "https://cloud.189.cn/t/share123",
        _ => unreachable!(),
    };
    crate::parser::CloudShareCandidate {
        provider,
        url: url.into(),
        access_code: Some("a123".into()),
    }
}
pub fn entry(plan: CloudSharePlan) -> CloudShareResolvedEntry {
    CloudShareResolvedEntry {
        provider: plan.provider,
        entry: plan.entries[0].clone(),
        plan,
    }
}
#[test]
fn all_seven_share_providers_are_registered_without_network_side_effects() {
    let credentials = crate::cloud::MemoryCredentialStore::default();
    let http = Http::new(vec![]);
    for provider in [
        CloudProvider::Quark,
        CloudProvider::Pikpak,
        CloudProvider::Baidu,
        CloudProvider::Tianyi,
        CloudProvider::Aliyun,
        CloudProvider::Xunlei,
        CloudProvider::Uc,
    ] {
        assert!(provider_share_adapter(provider, &credentials, &http).is_ok());
    }
    assert!(http.requests.lock().unwrap().is_empty());
}
