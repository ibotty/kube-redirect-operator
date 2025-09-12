use std::collections::{BTreeMap, HashSet};

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Failed to create Ingress: {0}")]
    IngressCreationFailed(#[source] kube::Error),
    #[error("Failed to create Ingress: {0}")]
    IngressDeletionFailed(#[source] kube::Error),
    #[error("Failed to update RedirectStatus: {0}")]
    StatusUpdateFailed(#[source] kube::Error),
}

impl Error {
    pub(crate) fn metric_label(&self) -> String {
        format!("{self:?}").to_lowercase()
    }
}

#[derive(CustomResource, Debug, Serialize, Deserialize, Default, Clone, JsonSchema)]
#[kube(
    group = "kube.ibotty.net",
    version = "v1alpha1",
    kind = "Redirect",
    namespaced
)]
#[kube(status = "RedirectStatus")]
#[serde(rename_all = "camelCase")]
pub struct RedirectSpec {
    pub hosts: HashSet<String>,
    pub to: RedirectTo,
    pub ingress: RedirectIngress,
}

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RedirectTo {
    pub uri: String,
    #[serde(default = "default_true")]
    pub include_request_uri: bool,
}

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RedirectIngress {
    #[serde(default = "default_true")]
    pub enabled: bool,

    #[serde(default)]
    pub tls: RedirectIngressTLS,

    #[serde(default)]
    pub ingress_class_name: Option<String>,

    pub annotations: Option<BTreeMap<String, String>>,
    pub labels: Option<BTreeMap<String, String>>,
}
#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RedirectIngressTLS {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub secret_name: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RedirectStatus {
    pub ingress: RedirectStatusIngress,
}

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RedirectStatusIngress {
    pub name: String,
    pub namespace: String,
}
