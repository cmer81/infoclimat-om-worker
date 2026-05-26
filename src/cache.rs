use std::env;

use aws_config::{BehaviorVersion, Region};
use aws_credential_types::Credentials;
use aws_sdk_s3::Client;
use bytes::Bytes;

#[derive(Clone)]
pub struct S3Cache {
    client: Client,
    bucket: String,
}

impl S3Cache {
    pub async fn from_env() -> anyhow::Result<Self> {
        let bucket = env::var("S3_BUCKET").unwrap_or_else(|_| "om-cumul-cache".to_string());
        let region = env::var("S3_REGION").unwrap_or_else(|_| "us-east-1".to_string());
        let endpoint = env::var("S3_ENDPOINT").ok();
        let access_key = env::var("S3_ACCESS_KEY_ID").ok();
        let secret_key = env::var("S3_SECRET_ACCESS_KEY").ok();

        let mut builder = aws_config::defaults(BehaviorVersion::latest()).region(Region::new(region));
        if let (Some(ak), Some(sk)) = (access_key.as_ref(), secret_key.as_ref()) {
            builder = builder.credentials_provider(Credentials::new(
                ak,
                sk,
                None,
                None,
                "infoclimat-om-worker",
            ));
        }
        let shared = builder.load().await;

        let mut s3_cfg = aws_sdk_s3::config::Builder::from(&shared);
        if let Some(ep) = endpoint {
            s3_cfg = s3_cfg.endpoint_url(ep).force_path_style(true);
        }
        let client = Client::from_conf(s3_cfg.build());

        Ok(Self { client, bucket })
    }

    /// Cheap existence check (HEAD object). Used on the redirect path so we
    /// don't waste bandwidth downloading bytes we won't send to the client.
    pub async fn exists(&self, key: &str) -> Result<bool, String> {
        match self.client.head_object().bucket(&self.bucket).key(key).send().await {
            Ok(_) => Ok(true),
            Err(e) => {
                let raw = format!("{e:?}");
                let svc_err = e.into_service_error();
                if svc_err.is_not_found() {
                    return Ok(false);
                }
                let code = svc_err.meta().code().unwrap_or("?");
                let msg = svc_err.meta().message().unwrap_or("?");
                Err(format!(
                    "head_object bucket={} key={} code={code} msg={msg} raw={raw}",
                    self.bucket, key
                ))
            }
        }
    }

    pub async fn get(&self, key: &str) -> Result<Option<Bytes>, String> {
        match self.client.get_object().bucket(&self.bucket).key(key).send().await {
            Ok(resp) => {
                let data = resp
                    .body
                    .collect()
                    .await
                    .map_err(|e| format!("read body: {e}"))?
                    .into_bytes();
                Ok(Some(data))
            }
            Err(e) => {
                let raw = format!("{e:?}");
                let svc_err = e.into_service_error();
                if svc_err.is_no_such_key() {
                    return Ok(None);
                }
                let code = svc_err.meta().code().unwrap_or("?");
                let msg = svc_err.meta().message().unwrap_or("?");
                let status = svc_err
                    .meta()
                    .extra("http_status")
                    .map(|v| format!("{v:?}"))
                    .unwrap_or_default();
                Err(format!(
                    "get_object bucket={} key={} code={code} status={status} msg={msg} raw={raw}",
                    self.bucket, key
                ))
            }
        }
    }

    pub async fn put(&self, key: &str, body: &Bytes) -> Result<(), String> {
        match self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .body(body.clone().into())
            .content_type("application/octet-stream")
            .cache_control("public, max-age=31536000, immutable")
            .send()
            .await
        {
            Ok(_) => Ok(()),
            Err(e) => {
                let raw = format!("{e:?}");
                let svc_err = e.into_service_error();
                let code = svc_err.meta().code().unwrap_or("?");
                let msg = svc_err.meta().message().unwrap_or("?");
                Err(format!(
                    "put_object bucket={} key={} code={code} msg={msg} raw={raw}",
                    self.bucket, key
                ))
            }
        }
    }
}
