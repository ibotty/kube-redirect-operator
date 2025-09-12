use std::{env, sync::Arc, time::Duration};

use crate::{metrics::Metrics, types::*};

use futures::StreamExt;
use k8s_openapi::api::networking::v1::{
    HTTPIngressPath, HTTPIngressRuleValue, Ingress, IngressBackend, IngressRule,
    IngressServiceBackend, IngressSpec, IngressTLS, ServiceBackendPort,
};
use kube::{
    Api, Client, ResourceExt,
    api::{ObjectMeta, Patch, PatchParams},
    runtime::{Config, Controller, controller::Action, finalizer, reflector::Store, watcher},
};
use serde_json::json;
use tokio::task::JoinHandle;
use tracing::{info, instrument, warn};

pub const REDIRECT_KUBE_SLUG: &str = "redirect.kube.ibotty.net";
pub const REDIRECT_KUBE_FINALIZER_SLUG: &str = "redirect.kube.ibotty.net/cleanup";

#[derive(Clone)]
pub struct Context {
    pub self_namespace: String,
    pub self_service_name: String,

    pub client: Client,
    pub api: Api<Redirect>,
    // pub diagnostics: Arc<RwLock<Diagnostics>>,
    pub metrics: Arc<Metrics>,
}

impl Context {
    pub async fn from_env() -> anyhow::Result<Self> {
        let self_namespace = env::var("SELF_NAMESPACE").unwrap_or("redirect-operator".to_string());
        let self_service_name =
            env::var("SELF_SERVICE_NAME").unwrap_or("redirect-operator".to_string());

        let client = Client::try_default().await?;

        let api = match env::var("WATCH_NAMESPACE") {
            Ok(ns) => Api::namespaced(client.clone(), &ns),
            Err(env::VarError::NotPresent) => Api::all(client.clone()),
            Err(e) => Err(e)?,
        };

        let metrics = Default::default();

        Ok(Self {
            client,
            api,
            metrics,
            self_namespace,
            self_service_name,
        })
    }
}

fn ingress_backend(service_name: impl ToString) -> IngressBackend {
    IngressBackend {
        resource: None,
        service: Some(IngressServiceBackend {
            name: service_name.to_string(),
            port: Some(ServiceBackendPort {
                name: None,
                number: Some(8080),
            }),
        }),
    }
}
fn ingress_for_redirect(ctx: &Arc<Context>, redirect: &Redirect) -> Ingress {
    let redirect_ingress = redirect.spec.ingress.clone();

    // cannot own across namespaces
    // let oref = redirect.controller_owner_ref(&()).unwrap();

    let tls = if redirect_ingress.tls.enabled {
        Some(vec![IngressTLS {
            hosts: Some(redirect.spec.hosts.clone().into_iter().collect()),
            secret_name: Some(
                redirect_ingress
                    .tls
                    .secret_name
                    .unwrap_or_else(|| format!("{}-tls-certs", redirect.name_any())),
            ),
        }])
    } else {
        None
    };
    let http_rule = Some(HTTPIngressRuleValue {
        paths: vec![HTTPIngressPath {
            backend: ingress_backend(&ctx.self_service_name),
            path: Some("/".to_string()),
            path_type: "Prefix".to_string(),
        }],
    });
    let rules = Some(
        redirect
            .spec
            .hosts
            .iter()
            .map(|host| IngressRule {
                host: Some(host.clone()),
                http: http_rule.clone(),
            })
            .collect(),
    );

    Ingress {
        metadata: ObjectMeta {
            name: Some(ingress_name_for_redirect(redirect)),
            namespace: Some(ctx.self_namespace.clone()),

            // cannot own across namespaces
            // owner_references: Some(vec![oref]),
            annotations: redirect_ingress.labels,
            labels: redirect_ingress.annotations,
            ..ObjectMeta::default()
        },
        spec: Some(IngressSpec {
            ingress_class_name: redirect_ingress.ingress_class_name,
            rules,
            tls,
            ..IngressSpec::default()
        }),
        status: None,
    }
}

pub fn ingress_name_for_redirect(redirect: &Redirect) -> String {
    format!(
        "{}.{}",
        &redirect.metadata.namespace.as_ref().unwrap(),
        &redirect.metadata.name.as_ref().unwrap()
    )
}

#[instrument(skip(ctx), fields(trace_id))]
pub async fn reconcile(
    redirect: Arc<Redirect>,
    ctx: Arc<Context>,
) -> Result<Action, finalizer::Error<Error>> {
    let ns = redirect.metadata.namespace.as_deref().unwrap();
    let api: Api<Redirect> = Api::namespaced(ctx.client.clone(), ns);
    finalizer(
        &api,
        REDIRECT_KUBE_FINALIZER_SLUG,
        redirect,
        |event| async {
            match event {
                finalizer::Event::Apply(redirect) => apply(redirect, ctx).await,
                finalizer::Event::Cleanup(redirect) => cleanup(redirect, ctx).await,
            }
        },
    )
    .await
}

#[instrument(skip(ctx), fields(trace_id))]
pub async fn cleanup(redirect: Arc<Redirect>, ctx: Arc<Context>) -> Result<Action, Error> {
    let ingress_api: Api<Ingress> = Api::namespaced(ctx.client.clone(), &ctx.self_namespace);

    let ingress_name = ingress_name_for_redirect(&redirect);
    ingress_api
        .delete(&ingress_name, &Default::default())
        .await
        .map_err(Error::IngressDeletionFailed)?;
    Ok(Action::requeue(Duration::from_secs(300)))
}

#[instrument(skip(ctx), fields(trace_id))]
pub async fn apply(redirect: Arc<Redirect>, ctx: Arc<Context>) -> Result<Action, Error> {
    let _timer = ctx.metrics.reconcile.count_and_measure();

    let ns = redirect.namespace().unwrap();
    let redirect_name = redirect.name_any();
    info!("Reconciling Redirect \"{}\" in {}", redirect_name, ns);

    let api: Api<Redirect> = Api::namespaced(ctx.client.clone(), &ns);

    let mut status = RedirectStatus::default();
    if redirect.spec.ingress.enabled {
        let ingress = ingress_for_redirect(&ctx, &redirect);
        let ingress_name = ingress.name_any();

        let ingress_api: Api<Ingress> = Api::namespaced(ctx.client.clone(), &ctx.self_namespace);

        ingress_api
            .patch(
                &ingress_name,
                &PatchParams::apply(REDIRECT_KUBE_SLUG),
                &Patch::Apply(ingress),
            )
            .await
            .map_err(Error::IngressCreationFailed)?;

        status.ingress = RedirectStatusIngress {
            name: ingress_name,
            namespace: ctx.self_namespace.clone(),
        };
    }

    api.patch_status(
        &redirect_name,
        &PatchParams::default(),
        &Patch::Merge(json!({"status": status})),
    )
    .await
    .map_err(Error::StatusUpdateFailed)?;

    Ok(Action::requeue(Duration::from_secs(300)))
}

pub async fn get_controller() -> anyhow::Result<(Store<Redirect>, Arc<Metrics>, JoinHandle<()>)> {
    let ctx = Context::from_env().await?;
    let controller_config = Config::default().concurrency(2);

    let controller = Controller::new(ctx.api.clone(), watcher::Config::default())
        // cannot own across namespaces
        // .owns(ctx.ingress_api.clone(), watcher::Config::default())
        .with_config(controller_config)
        // .reconcile_all_on(reload_rx.map(|_| (())))
        .shutdown_on_signal();

    // r/o store for redirects
    let store = controller.store();
    let metrics = ctx.metrics.clone();

    let future = controller
        .run(reconcile, error_policy, Arc::new(ctx))
        .for_each(|res| async move {
            match res {
                Ok(o) => info!("reconciled {:?}", o),
                Err(e) => warn!("reconcile failed: {:?}", e),
            }
        });

    Ok((store, metrics, tokio::spawn(future)))
}

fn error_policy(
    redirect: Arc<Redirect>,
    error: &finalizer::Error<Error>,
    ctx: Arc<Context>,
) -> Action {
    ctx.metrics.reconcile.set_failure(&redirect, error);

    // just requeue
    Action::requeue(Duration::from_secs(1))
}
