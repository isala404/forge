# File Storage

> *Handling file uploads and storage*

---

## Overview

FORGE supports multiple storage backends:

| Backend | Use Case | Configuration |
|---------|----------|---------------|
| **PostgreSQL** | Small files, simplicity | Default |
| **S3** | Large files, scalability | Optional |
| **Minio** | Self-hosted S3 | Optional |
| **R2** | Cloudflare | Optional |

---

## PostgreSQL Storage (Default)

Small files stored directly in PostgreSQL:

```rust
#[forge::mutation]
pub async fn upload_avatar(ctx: &MutationContext, file: UploadedFile) -> Result<String> {
    // Stored in PostgreSQL large objects
    let url = ctx.storage.put("avatars", &file).await?;
    Ok(url)
}
```

---

## S3 Storage

```toml
# forge.toml
[storage]
backend = "s3"
bucket = "my-app-uploads"
region = "us-east-1"
access_key = "${AWS_ACCESS_KEY}"
secret_key = "${AWS_SECRET_KEY}"
```

```rust
#[forge::mutation]
pub async fn upload_file(ctx: &MutationContext, file: UploadedFile) -> Result<String> {
    // Automatically uses S3
    let url = ctx.storage.put("uploads", &file).await?;
    Ok(url)
}
```

---

## Presigned URLs

For large files, use presigned URLs:

```rust
#[forge::query]
pub async fn get_upload_url(ctx: &QueryContext, filename: &str) -> Result<PresignedUrl> {
    ctx.storage.presigned_put("uploads", filename, Duration::minutes(15)).await
}

#[forge::query]
pub async fn get_download_url(ctx: &QueryContext, key: &str) -> Result<String> {
    ctx.storage.presigned_get(key, Duration::hours(1)).await
}
```

---

## Configuration Options

```toml
[storage]
backend = "s3"           # postgres, s3, minio, r2
bucket = "my-bucket"
region = "us-east-1"
endpoint = "..."         # Custom endpoint (Minio/R2)
access_key = "${KEY}"
secret_key = "${SECRET}"
max_file_size = "100MB"
allowed_types = ["image/*", "application/pdf"]
```

---

## Related Documentation

- [Configuration](CONFIGURATION.md) — Full config reference
- [Security](SECURITY.md) — Access control
