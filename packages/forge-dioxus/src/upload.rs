/// File upload value used by generated mutation clients.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ForgeUpload {
    source: UploadSource,
    file_name: Option<String>,
    content_type: Option<String>,
}

#[cfg(target_arch = "wasm32")]
#[derive(Debug, Clone)]
enum UploadSource {
    File(web_sys::File),
    Blob(web_sys::Blob),
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone)]
enum UploadSource {
    Bytes(Vec<u8>),
}

impl ForgeUpload {
    #[must_use]
    pub fn with_file_name(mut self, file_name: impl Into<String>) -> Self {
        self.file_name = Some(file_name.into());
        self
    }

    #[must_use]
    pub fn with_content_type(mut self, content_type: impl Into<String>) -> Self {
        self.content_type = Some(content_type.into());
        self
    }

    #[must_use]
    pub fn file_name(&self) -> Option<&str> {
        self.file_name.as_deref()
    }

    #[must_use]
    pub fn content_type(&self) -> Option<&str> {
        self.content_type.as_deref()
    }
}

#[cfg(target_arch = "wasm32")]
impl ForgeUpload {
    pub fn from_file(file: web_sys::File) -> Self {
        let file_name = Some(file.name());
        Self {
            source: UploadSource::File(file),
            file_name,
            content_type: None,
        }
    }

    pub fn from_blob(blob: web_sys::Blob) -> Self {
        Self {
            source: UploadSource::Blob(blob),
            file_name: None,
            content_type: None,
        }
    }

    pub fn from_blob_with_name(blob: web_sys::Blob, file_name: impl Into<String>) -> Self {
        Self::from_blob(blob).with_file_name(file_name)
    }

    /// Append this upload to browser multipart form data.
    pub fn append_to_form(
        &self,
        form: &web_sys::FormData,
        field_name: &str,
    ) -> Result<(), crate::ForgeClientError> {
        match &self.source {
            UploadSource::File(file) => {
                let default_name = file.name();
                let file_name = self.file_name.as_deref().unwrap_or(default_name.as_str());
                form.append_with_blob_and_filename(field_name, file, file_name)
                    .map_err(|_| crate::ForgeClientError::new("UPLOAD_FAILED", "Failed to append file", None))
            }
            UploadSource::Blob(blob) => match &self.file_name {
                Some(file_name) => form
                    .append_with_blob_and_filename(field_name, blob, file_name)
                    .map_err(|_| crate::ForgeClientError::new("UPLOAD_FAILED", "Failed to append blob", None)),
                None => form
                    .append_with_blob(field_name, blob)
                    .map_err(|_| crate::ForgeClientError::new("UPLOAD_FAILED", "Failed to append blob", None)),
            },
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl From<web_sys::File> for ForgeUpload {
    fn from(value: web_sys::File) -> Self {
        Self::from_file(value)
    }
}

#[cfg(target_arch = "wasm32")]
impl From<web_sys::Blob> for ForgeUpload {
    fn from(value: web_sys::Blob) -> Self {
        Self::from_blob(value)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl ForgeUpload {
    pub fn from_bytes(bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            source: UploadSource::Bytes(bytes.into()),
            file_name: None,
            content_type: None,
        }
    }

    pub fn from_bytes_with_name(
        bytes: impl Into<Vec<u8>>,
        file_name: impl Into<String>,
    ) -> Self {
        Self::from_bytes(bytes).with_file_name(file_name)
    }

    pub fn into_part(self) -> Result<reqwest::multipart::Part, crate::ForgeClientError> {
        let UploadSource::Bytes(bytes) = self.source;
        let mut part = reqwest::multipart::Part::bytes(bytes);
        if let Some(file_name) = self.file_name {
            part = part.file_name(file_name);
        }
        if let Some(content_type) = self.content_type {
            part = part
                .mime_str(&content_type)
                .map_err(|err| crate::ForgeClientError::new("UPLOAD_FAILED", err.to_string(), None))?;
        }
        Ok(part)
    }
}
