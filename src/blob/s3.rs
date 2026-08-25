use super::common;
use super::{
    Blob, BlobInfo, BlobReader, BlobSummary, ConditionalGet, DEFAULT_CONTENT_TYPE, ListPage,
    MAX_MULTIPART_PARTS, MAX_OBJECT_BYTES, MAX_PRESIGN_EXPIRES, MultipartPart, MultipartUpload,
    NativePresign, ProxyPresign, PutOpts, PutPrecondition, S3Encryption,
};
use crate::backend::{BackendLifecycle, Primitive};
use crate::config::S3BlobConfig;
use crate::error::{ForgeError, Result};
use crate::types::Cursor;
use async_trait::async_trait;
use aws_config::{BehaviorVersion, Region};
use aws_credential_types::Credentials;
use aws_sdk_s3::Client;
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart, ServerSideEncryption};
use aws_smithy_types::error::metadata::ProvideErrorMetadata;
use aws_smithy_types::retry::RetryConfig;
use aws_smithy_types::timeout::TimeoutConfig;
use bytes::Bytes;
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};
use tokio::io::AsyncReadExt;
use uuid::Uuid;

const MULTIPART_PART_BYTES: usize = 8 * 1024 * 1024;
const CHECKSUM_METADATA_KEY: &str = "forge-checksum-sha256";

pub(crate) struct S3Blob {
    client: Client,
    bucket: String,
    key_prefix: String,
    shared: common::Shared,
}

impl S3Blob {
    pub(crate) async fn new(
        config: S3BlobConfig,
        namespace: String,
        secret: Option<Vec<u8>>,
        base_url: String,
    ) -> Result<Self> {
        let timeout = TimeoutConfig::builder()
            .connect_timeout(config.connect_timeout)
            .operation_timeout(config.request_timeout)
            .build();
        let retry = RetryConfig::standard().with_max_attempts(config.max_retries.saturating_add(1));
        let mut loader = aws_config::defaults(BehaviorVersion::latest())
            .region(Region::new(config.region.clone()))
            .timeout_config(timeout)
            .retry_config(retry);
        if let (Some(access_key), Some(secret_key)) =
            (config.access_key.clone(), config.secret_key.clone())
        {
            loader = loader.credentials_provider(Credentials::new(
                access_key,
                secret_key,
                config.session_token.clone(),
                None,
                "forge.toml",
            ));
        }
        let shared_config = loader.load().await;
        let mut service =
            aws_sdk_s3::config::Builder::from(&shared_config).force_path_style(config.path_style);
        if let Some(endpoint) = &config.endpoint {
            service = service.endpoint_url(endpoint);
        }
        let key_prefix = [config.prefix.as_str(), namespace.as_str()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("/");
        let blob = Self {
            client: Client::from_conf(service.build()),
            bucket: config.bucket,
            key_prefix,
            shared: common::Shared::new(namespace, secret, base_url),
        };
        blob.probe().await?;
        Ok(blob)
    }

    fn physical(&self, key: &str) -> String {
        if self.key_prefix.is_empty() {
            key.to_string()
        } else if key.is_empty() {
            format!("{}/", self.key_prefix)
        } else {
            format!("{}/{key}", self.key_prefix)
        }
    }

    fn logical<'a>(&self, key: &'a str) -> &'a str {
        if self.key_prefix.is_empty() {
            key
        } else {
            key.strip_prefix(&self.key_prefix)
                .and_then(|value| value.strip_prefix('/'))
                .unwrap_or(key)
        }
    }

    async fn probe(&self) -> Result<()> {
        self.client
            .head_bucket()
            .bucket(&self.bucket)
            .send()
            .await
            .map_err(|error| {
                let code = error
                    .as_service_error()
                    .and_then(ProvideErrorMetadata::code)
                    .unwrap_or("unknown");
                ForgeError::config(format!(
                    "S3 credentials cannot access the configured bucket (provider code: {code})"
                ))
            })?;

        let key = self.physical(&format!(".forge-probe/{}", Uuid::new_v4()));
        let created = self
            .client
            .create_multipart_upload()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await
            .map_err(|_| ForgeError::config("S3 credentials cannot initiate multipart uploads"))?;
        let upload_id = created
            .upload_id()
            .ok_or_else(|| ForgeError::backend("S3 write probe returned no multipart upload id"))?;
        self.client
            .abort_multipart_upload()
            .bucket(&self.bucket)
            .key(key)
            .upload_id(upload_id)
            .send()
            .await
            .map_err(|_| ForgeError::config("S3 credentials cannot abort multipart uploads"))?;
        Ok(())
    }

    async fn abort_best_effort(&self, key: &str, upload_id: &str) {
        let _ = self
            .client
            .abort_multipart_upload()
            .bucket(&self.bucket)
            .key(key)
            .upload_id(upload_id)
            .send()
            .await;
    }

    fn native_ticket(
        request: aws_sdk_s3::presigning::PresignedRequest,
        expires: Duration,
        mut constraints: BTreeMap<String, String>,
    ) -> NativePresign {
        constraints.insert("bearer_credential".to_string(), "true".to_string());
        NativePresign {
            url: request.uri().to_string(),
            method: request.method().to_string(),
            expires_epoch: common::unix_secs(SystemTime::now() + expires),
            required_headers: request
                .headers()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
            constraints,
        }
    }

    fn metadata_with_checksum(data: &[u8], mut opts: PutOpts) -> (PutOpts, String) {
        let checksum = crate::util::sha256_hex(data);
        opts.metadata
            .insert(CHECKSUM_METADATA_KEY.to_string(), checksum.clone());
        (opts, checksum)
    }
}

fn s3_error(
    context: &'static str,
    error: impl std::error::Error + Send + Sync + 'static,
) -> ForgeError {
    ForgeError::backend_with(context, false, error)
}

fn classified_s3_error(
    context: &'static str,
    code: Option<&str>,
    error: impl std::error::Error + Send + Sync + 'static,
) -> ForgeError {
    match code {
        Some("PreconditionFailed" | "ConditionalRequestConflict") => {
            ForgeError::precondition("S3 write precondition failed")
        }
        Some("SlowDown" | "ServiceUnavailable" | "InternalError" | "RequestTimeout") => {
            ForgeError::backend_with(context, true, error)
        }
        _ => s3_error(context, error),
    }
}

macro_rules! classify_s3 {
    ($context:literal, $error:expr) => {{
        let error = $error;
        let code = error
            .as_service_error()
            .and_then(ProvideErrorMetadata::code)
            .map(str::to_owned);
        classified_s3_error($context, code.as_deref(), error)
    }};
}

fn checked_expiry(expires: Duration) -> Result<PresigningConfig> {
    if expires.is_zero() {
        return Err(ForgeError::invalid("presign expiry must be positive"));
    }
    if expires > MAX_PRESIGN_EXPIRES {
        return Err(ForgeError::limit("presign expiry exceeds seven days"));
    }
    PresigningConfig::expires_in(expires)
        .map_err(|error| ForgeError::backend_with("invalid native presign expiry", false, error))
}

#[async_trait]
impl Blob for S3Blob {
    async fn put(&self, key: &str, data: Bytes, opts: PutOpts) -> Result<()> {
        common::check_key(key)?;
        common::check_put(&data, &opts)?;
        let checksum_header = opts
            .checksum_sha256
            .as_deref()
            .map(common::sha256_base64)
            .transpose()?;
        let (opts, _) = Self::metadata_with_checksum(&data, opts);
        let physical = self.physical(key);
        let mut request = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(physical)
            .body(ByteStream::from(data))
            .content_type(
                opts.content_type
                    .clone()
                    .unwrap_or_else(|| DEFAULT_CONTENT_TYPE.to_string()),
            )
            .set_metadata(Some(opts.metadata.into_iter().collect()));
        request = request
            .set_cache_control(opts.cache_control)
            .set_content_disposition(opts.content_disposition)
            .set_checksum_sha256(checksum_header);
        request = match opts.s3_encryption {
            Some(S3Encryption::S3Managed) => {
                request.server_side_encryption(ServerSideEncryption::Aes256)
            }
            Some(S3Encryption::Kms { key_id }) => request
                .server_side_encryption(ServerSideEncryption::AwsKms)
                .set_ssekms_key_id(key_id),
            None => request,
        };
        request = match opts.precondition {
            Some(PutPrecondition::CreateOnly) => request.if_none_match("*"),
            Some(PutPrecondition::MatchVersion(etag)) => request.if_match(etag),
            None => request,
        };
        request
            .send()
            .await
            .map_err(|error| classify_s3!("S3 put failed", error))?;
        Ok(())
    }

    async fn put_stream(
        &self,
        key: &str,
        mut reader: BlobReader,
        content_length: u64,
        opts: PutOpts,
    ) -> Result<()> {
        common::check_key(key)?;
        common::check_put(&[], &opts)?;
        if content_length <= MAX_OBJECT_BYTES as u64 {
            let capacity = usize::try_from(content_length)
                .map_err(|_| ForgeError::limit("object length exceeds this platform"))?;
            let mut body = Vec::with_capacity(capacity);
            reader
                .take(content_length.saturating_add(1))
                .read_to_end(&mut body)
                .await
                .map_err(|error| s3_error("could not read S3 upload stream", error))?;
            if body.len() as u64 != content_length {
                return Err(ForgeError::invalid(
                    "blob stream length does not match content_length",
                ));
            }
            return self.put(key, Bytes::from(body), opts).await;
        }
        if opts.checksum_sha256.is_some() {
            return Err(ForgeError::invalid(
                "verify a completed multipart stream with verify_checksum_sha256",
            ));
        }

        let physical = self.physical(key);
        let mut create = self
            .client
            .create_multipart_upload()
            .bucket(&self.bucket)
            .key(&physical)
            .content_type(
                opts.content_type
                    .clone()
                    .unwrap_or_else(|| DEFAULT_CONTENT_TYPE.to_string()),
            )
            .set_metadata(Some(opts.metadata.into_iter().collect()))
            .set_cache_control(opts.cache_control)
            .set_content_disposition(opts.content_disposition);
        create = match opts.s3_encryption {
            Some(S3Encryption::S3Managed) => {
                create.server_side_encryption(ServerSideEncryption::Aes256)
            }
            Some(S3Encryption::Kms { key_id }) => create
                .server_side_encryption(ServerSideEncryption::AwsKms)
                .set_ssekms_key_id(key_id),
            None => create,
        };
        let created = create
            .send()
            .await
            .map_err(|error| classify_s3!("S3 multipart initialization failed", error))?;
        let upload_id = created
            .upload_id()
            .ok_or_else(|| ForgeError::backend("S3 multipart upload returned no upload id"))?
            .to_string();

        let mut remaining = content_length;
        let mut number = 1i32;
        let mut parts = Vec::new();
        while remaining > 0 {
            let wanted = remaining.min(MULTIPART_PART_BYTES as u64);
            let mut body =
                Vec::with_capacity(usize::try_from(wanted).unwrap_or(MULTIPART_PART_BYTES));
            if let Err(error) = (&mut reader).take(wanted).read_to_end(&mut body).await {
                self.abort_best_effort(&physical, &upload_id).await;
                return Err(s3_error("could not read S3 multipart stream", error));
            }
            if body.len() as u64 != wanted {
                self.abort_best_effort(&physical, &upload_id).await;
                return Err(ForgeError::invalid(
                    "blob stream ended before content_length",
                ));
            }
            let uploaded = self
                .client
                .upload_part()
                .bucket(&self.bucket)
                .key(&physical)
                .upload_id(&upload_id)
                .part_number(number)
                .body(ByteStream::from(body))
                .send()
                .await;
            let uploaded = match uploaded {
                Ok(value) => value,
                Err(error) => {
                    self.abort_best_effort(&physical, &upload_id).await;
                    return Err(classify_s3!("S3 multipart part upload failed", error));
                }
            };
            let part = CompletedPart::builder()
                .set_e_tag(uploaded.e_tag().map(str::to_string))
                .part_number(number)
                .build();
            parts.push(part);
            remaining -= wanted;
            number += 1;
        }

        let upload = CompletedMultipartUpload::builder()
            .set_parts(Some(parts))
            .build();
        let mut complete = self
            .client
            .complete_multipart_upload()
            .bucket(&self.bucket)
            .key(&physical)
            .upload_id(&upload_id)
            .multipart_upload(upload);
        complete = match opts.precondition {
            Some(PutPrecondition::CreateOnly) => complete.if_none_match("*"),
            Some(PutPrecondition::MatchVersion(etag)) => complete.if_match(etag),
            None => complete,
        };
        if let Err(error) = complete.send().await {
            self.abort_best_effort(&physical, &upload_id).await;
            return Err(classify_s3!("S3 multipart completion failed", error));
        }
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Option<Bytes>> {
        common::check_key(key)?;
        let output = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(self.physical(key))
            .send()
            .await;
        let output = match output {
            Ok(value) => value,
            Err(error)
                if error
                    .as_service_error()
                    .is_some_and(|value| value.is_no_such_key()) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(s3_error("S3 get failed", error)),
        };
        if output.content_length().unwrap_or(0) > MAX_OBJECT_BYTES as i64 {
            return Err(ForgeError::limit(
                "object exceeds the 50 MiB buffered read limit; use open",
            ));
        }
        let body = output
            .body
            .collect()
            .await
            .map_err(|error| s3_error("S3 response stream failed", error))?;
        Ok(Some(body.into_bytes()))
    }

    async fn get_if(
        &self,
        key: &str,
        if_match: Option<&str>,
        if_none_match: Option<&str>,
    ) -> Result<ConditionalGet> {
        common::check_key(key)?;
        common::check_get_conditions(if_match, if_none_match)?;
        let mut request = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(self.physical(key));
        if let Some(version) = if_match {
            request = request.if_match(version);
        }
        if let Some(version) = if_none_match {
            request = request.if_none_match(version);
        }
        let output = match request.send().await {
            Ok(output) => output,
            Err(error)
                if error
                    .as_service_error()
                    .is_some_and(|value| value.is_no_such_key()) =>
            {
                return Ok(ConditionalGet::Missing);
            }
            Err(error)
                if error
                    .raw_response()
                    .is_some_and(|response| response.status().as_u16() == 304) =>
            {
                return Ok(ConditionalGet::NotModified {
                    etag: if_none_match.unwrap_or_default().to_string(),
                });
            }
            Err(error)
                if error
                    .raw_response()
                    .is_some_and(|response| response.status().as_u16() == 412) =>
            {
                return Err(ForgeError::precondition("blob read version does not match"));
            }
            Err(error) => return Err(s3_error("S3 conditional get failed", error)),
        };
        if output.content_length().unwrap_or(0) > MAX_OBJECT_BYTES as i64 {
            return Err(ForgeError::limit(
                "object exceeds the 50 MiB buffered read limit; use open",
            ));
        }
        let etag = output.e_tag().unwrap_or_default().to_string();
        let body = output
            .body
            .collect()
            .await
            .map_err(|error| s3_error("S3 response stream failed", error))?
            .into_bytes();
        Ok(ConditionalGet::Found { body, etag })
    }

    async fn open(&self, key: &str) -> Result<Option<BlobReader>> {
        common::check_key(key)?;
        let output = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(self.physical(key))
            .send()
            .await;
        match output {
            Ok(value) => Ok(Some(Box::pin(value.body.into_async_read()))),
            Err(error)
                if error
                    .as_service_error()
                    .is_some_and(|value| value.is_no_such_key()) =>
            {
                Ok(None)
            }
            Err(error) => Err(s3_error("S3 open failed", error)),
        }
    }

    async fn get_range(&self, key: &str, start: u64, end: u64) -> Result<Option<Bytes>> {
        common::check_key(key)?;
        if end < start {
            return Err(ForgeError::invalid("range end must be at least start"));
        }
        if end.saturating_sub(start).saturating_add(1) > MAX_OBJECT_BYTES as u64 {
            return Err(ForgeError::limit(
                "range exceeds the 50 MiB buffered read limit",
            ));
        }
        let output = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(self.physical(key))
            .range(format!("bytes={start}-{end}"))
            .send()
            .await;
        let output = match output {
            Ok(value) => value,
            Err(error)
                if error
                    .as_service_error()
                    .is_some_and(|value| value.is_no_such_key()) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(s3_error("S3 range read failed", error)),
        };
        Ok(Some(
            output
                .body
                .collect()
                .await
                .map_err(|error| s3_error("S3 range stream failed", error))?
                .into_bytes(),
        ))
    }

    async fn head(&self, key: &str) -> Result<Option<BlobInfo>> {
        common::check_key(key)?;
        let output = self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(self.physical(key))
            .send()
            .await;
        let output = match output {
            Ok(value) => value,
            Err(error)
                if error
                    .as_service_error()
                    .is_some_and(|value| value.is_not_found()) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(s3_error("S3 head failed", error)),
        };
        let modified = output
            .last_modified()
            .cloned()
            .and_then(|value| SystemTime::try_from(value).ok())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let mut metadata: BTreeMap<String, String> = output
            .metadata()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect();
        let checksum_sha256 = metadata.remove(CHECKSUM_METADATA_KEY);
        Ok(Some(
            BlobInfo::new(
                key.to_string(),
                u64::try_from(output.content_length().unwrap_or(0)).unwrap_or(0),
                output
                    .content_type()
                    .unwrap_or(DEFAULT_CONTENT_TYPE)
                    .to_string(),
                output.e_tag().unwrap_or_default().to_string(),
                modified,
                metadata,
            )
            .with_storage_metadata(
                output.cache_control().map(str::to_string),
                output.content_disposition().map(str::to_string),
                checksum_sha256,
                output
                    .server_side_encryption()
                    .map(|value| value.as_str().to_string()),
            ),
        ))
    }

    async fn create_multipart(&self, key: &str, opts: PutOpts) -> Result<MultipartUpload> {
        common::check_key(key)?;
        common::check_put(&[], &opts)?;
        if opts.checksum_sha256.is_some() {
            return Err(ForgeError::invalid(
                "verify a completed multipart upload with verify_checksum_sha256",
            ));
        }
        let physical = self.physical(key);
        let mut request = self
            .client
            .create_multipart_upload()
            .bucket(&self.bucket)
            .key(&physical)
            .content_type(
                opts.content_type
                    .clone()
                    .unwrap_or_else(|| DEFAULT_CONTENT_TYPE.to_string()),
            )
            .set_metadata(Some(opts.metadata.into_iter().collect()))
            .set_cache_control(opts.cache_control)
            .set_content_disposition(opts.content_disposition);
        request = match opts.s3_encryption {
            Some(S3Encryption::S3Managed) => {
                request.server_side_encryption(ServerSideEncryption::Aes256)
            }
            Some(S3Encryption::Kms { key_id }) => request
                .server_side_encryption(ServerSideEncryption::AwsKms)
                .set_ssekms_key_id(key_id),
            None => request,
        };
        let created = request
            .send()
            .await
            .map_err(|error| classify_s3!("S3 multipart initialization failed", error))?;
        let upload_id = created
            .upload_id()
            .ok_or_else(|| ForgeError::backend("S3 multipart upload returned no upload id"))?
            .to_string();
        Ok(MultipartUpload {
            key: key.to_string(),
            upload_id,
            precondition: opts.precondition,
        })
    }

    async fn upload_part(
        &self,
        upload: &MultipartUpload,
        part_number: u32,
        body: Bytes,
    ) -> Result<MultipartPart> {
        common::check_key(&upload.key)?;
        if part_number == 0 || part_number > MAX_MULTIPART_PARTS {
            return Err(ForgeError::invalid(
                "multipart part number must be 1..=10000",
            ));
        }
        if body.is_empty() || body.len() > MAX_OBJECT_BYTES {
            return Err(ForgeError::limit(
                "multipart part must be between 1 byte and 50 MiB",
            ));
        }
        let size = body.len() as u64;
        let uploaded = self
            .client
            .upload_part()
            .bucket(&self.bucket)
            .key(self.physical(&upload.key))
            .upload_id(&upload.upload_id)
            .part_number(i32::try_from(part_number).unwrap_or(i32::MAX))
            .body(ByteStream::from(body))
            .send()
            .await
            .map_err(|error| classify_s3!("S3 multipart part upload failed", error))?;
        Ok(MultipartPart {
            part_number,
            etag: uploaded.e_tag().unwrap_or_default().to_string(),
            size,
        })
    }

    async fn complete_multipart(
        &self,
        upload: &MultipartUpload,
        parts: Vec<MultipartPart>,
    ) -> Result<BlobInfo> {
        if parts.is_empty() || parts.len() > MAX_MULTIPART_PARTS as usize {
            return Err(ForgeError::invalid(
                "multipart completion requires 1..=10000 parts",
            ));
        }
        let mut previous = 0;
        let mut completed = Vec::with_capacity(parts.len());
        for part in parts {
            if part.part_number <= previous || part.part_number > MAX_MULTIPART_PARTS {
                return Err(ForgeError::invalid(
                    "multipart parts must be strictly ordered by part number",
                ));
            }
            previous = part.part_number;
            completed.push(
                CompletedPart::builder()
                    .e_tag(part.etag)
                    .part_number(i32::try_from(part.part_number).unwrap_or(i32::MAX))
                    .build(),
            );
        }
        let body = CompletedMultipartUpload::builder()
            .set_parts(Some(completed))
            .build();
        let mut request = self
            .client
            .complete_multipart_upload()
            .bucket(&self.bucket)
            .key(self.physical(&upload.key))
            .upload_id(&upload.upload_id)
            .multipart_upload(body);
        request = match &upload.precondition {
            Some(PutPrecondition::CreateOnly) => request.if_none_match("*"),
            Some(PutPrecondition::MatchVersion(etag)) => request.if_match(etag),
            None => request,
        };
        request
            .send()
            .await
            .map_err(|error| classify_s3!("S3 multipart completion failed", error))?;
        self.head(&upload.key)
            .await?
            .ok_or_else(|| ForgeError::backend("completed multipart blob is not readable"))
    }

    async fn abort_multipart(&self, upload: &MultipartUpload) -> Result<()> {
        common::check_key(&upload.key)?;
        let result = self
            .client
            .abort_multipart_upload()
            .bucket(&self.bucket)
            .key(self.physical(&upload.key))
            .upload_id(&upload.upload_id)
            .send()
            .await;
        if let Err(error) = result {
            let absent = error
                .as_service_error()
                .and_then(ProvideErrorMetadata::code)
                .is_some_and(|code| code == "NoSuchUpload");
            if !absent {
                return Err(classify_s3!("S3 multipart abort failed", error));
            }
        }
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<()> {
        common::check_key(key)?;
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(self.physical(key))
            .send()
            .await
            .map_err(|error| s3_error("S3 delete failed", error))?;
        Ok(())
    }

    async fn list(&self, prefix: &str, cursor: Option<Cursor>, limit: u32) -> Result<ListPage> {
        let output = self
            .client
            .list_objects_v2()
            .bucket(&self.bucket)
            .prefix(self.physical(prefix))
            .set_continuation_token(cursor.map(|value| value.token().to_string()))
            .max_keys(i32::try_from(limit.clamp(1, 1000)).unwrap_or(1000))
            .send()
            .await
            .map_err(|error| s3_error("S3 list failed", error))?;
        let mut items = Vec::with_capacity(output.contents().len());
        for object in output.contents() {
            let modified = object
                .last_modified()
                .cloned()
                .and_then(|value| SystemTime::try_from(value).ok())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            items.push(BlobSummary::new(
                self.logical(object.key().unwrap_or_default()).to_string(),
                u64::try_from(object.size().unwrap_or(0)).unwrap_or(0),
                object.e_tag().unwrap_or_default().to_string(),
                modified,
            ));
        }
        Ok(ListPage::new(
            items,
            output
                .next_continuation_token()
                .map(|value| Cursor::from_token(value.to_string())),
        ))
    }

    async fn presign_upload(
        &self,
        key: &str,
        expires: Duration,
        max_bytes: u64,
    ) -> Result<ProxyPresign> {
        self.shared.presign_upload(key, expires, max_bytes).await
    }

    async fn presign_download(&self, key: &str, expires: Duration) -> Result<ProxyPresign> {
        self.shared.presign_download(key, expires).await
    }

    async fn presign_native_get(&self, key: &str, expires: Duration) -> Result<NativePresign> {
        common::check_key(key)?;
        let request = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(self.physical(key))
            .presigned(checked_expiry(expires)?)
            .await
            .map_err(|error| s3_error("S3 GET presign failed", error))?;
        Ok(Self::native_ticket(request, expires, BTreeMap::new()))
    }

    async fn presign_native_put(
        &self,
        key: &str,
        expires: Duration,
        opts: PutOpts,
    ) -> Result<NativePresign> {
        common::check_key(key)?;
        common::check_put(&[], &opts)?;
        let checksum_header = opts
            .checksum_sha256
            .as_deref()
            .map(common::sha256_base64)
            .transpose()?;
        let mut builder = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(self.physical(key))
            .content_type(
                opts.content_type
                    .unwrap_or_else(|| DEFAULT_CONTENT_TYPE.to_string()),
            )
            .set_metadata(Some(opts.metadata.into_iter().collect()))
            .set_cache_control(opts.cache_control)
            .set_content_disposition(opts.content_disposition)
            .set_checksum_sha256(checksum_header);
        builder = match opts.s3_encryption {
            Some(S3Encryption::S3Managed) => {
                builder.server_side_encryption(ServerSideEncryption::Aes256)
            }
            Some(S3Encryption::Kms { key_id }) => builder
                .server_side_encryption(ServerSideEncryption::AwsKms)
                .set_ssekms_key_id(key_id),
            None => builder,
        };
        builder = match opts.precondition {
            Some(PutPrecondition::CreateOnly) => builder.if_none_match("*"),
            Some(PutPrecondition::MatchVersion(etag)) => builder.if_match(etag),
            None => builder,
        };
        let request = builder
            .presigned(checked_expiry(expires)?)
            .await
            .map_err(|error| s3_error("S3 PUT presign failed", error))?;
        let mut constraints = BTreeMap::new();
        constraints.insert(
            "maximum_body_size".to_string(),
            "not_portably_enforced".to_string(),
        );
        Ok(Self::native_ticket(request, expires, constraints))
    }

    async fn verify_presigned(
        &self,
        method: &str,
        key: &str,
        expires_epoch: i64,
        max_bytes: u64,
        sig: &str,
    ) -> Result<bool> {
        self.shared
            .verify_presigned(method, key, expires_epoch, max_bytes, sig)
    }
}

#[async_trait]
impl BackendLifecycle for S3Blob {
    fn name(&self) -> &'static str {
        "s3"
    }

    fn primitive(&self) -> Primitive {
        Primitive::Blob
    }

    fn caveats(&self) -> &'static str {
        "list is ordered but not a point-in-time snapshot"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_retryable_provider_errors() {
        let error = classified_s3_error(
            "S3 test failed",
            Some("SlowDown"),
            std::io::Error::other("provider asked us to retry"),
        );
        assert!(error.is_retryable());
    }

    #[test]
    fn classifies_provider_preconditions() {
        let error = classified_s3_error(
            "S3 test failed",
            Some("PreconditionFailed"),
            std::io::Error::other("condition did not match"),
        );
        assert!(matches!(error, ForgeError::Precondition(_)));
    }

    #[test]
    fn clock_skew_is_not_blindly_retried() {
        let error = classified_s3_error(
            "S3 test failed",
            Some("RequestTimeTooSkewed"),
            std::io::Error::other("request clock is outside the provider window"),
        );
        assert!(!error.is_retryable());
        assert!(matches!(error, ForgeError::Backend { .. }));
    }
}
