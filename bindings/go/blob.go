package forge

import (
	"bytes"
	"context"
	"crypto/sha256"
	"crypto/subtle"
	"encoding/base64"
	"encoding/hex"
	"fmt"
	"io"
	"net/url"
	"sort"
	"strconv"
	"strings"
	"time"
)

const MaxBufferedBlobBytes = 50 * 1024 * 1024

type PutOptions struct {
	ContentType        string
	Metadata           map[string]string
	CacheControl       string
	ContentDisposition string
	ChecksumSHA256     string
	S3Encryption       *S3Encryption
	Precondition       *PutPrecondition
}

type S3Encryption struct {
	Algorithm string
	KMSKeyID  string
}

func S3ManagedEncryption() *S3Encryption {
	return &S3Encryption{Algorithm: "AES256"}
}

func KMSManagedEncryption(keyID string) *S3Encryption {
	return &S3Encryption{Algorithm: "aws:kms", KMSKeyID: keyID}
}

type PutPrecondition struct {
	createOnly bool
	version    string
}

func CreateOnly() *PutPrecondition {
	return &PutPrecondition{createOnly: true}
}

func MatchVersion(etag string) *PutPrecondition {
	return &PutPrecondition{version: etag}
}

type memoryBlob struct {
	body         []byte
	contentType  string
	etag         string
	lastModified time.Time
	metadata     map[string]string
	cacheControl string
	disposition  string
	checksum     string
}

func (f *Forge) BlobPut(ctx context.Context, key string, body []byte, options PutOptions) error {
	if err := f.ready(ctx, "blob.put"); err != nil {
		return err
	}
	if err := validateBlobKey("blob.put", key); err != nil {
		return err
	}
	if len(body) > MaxBufferedBlobBytes {
		return forgeError(CodeLimit, "blob.put", "buffered body exceeds 50 MiB")
	}
	if err := validatePutOptions("blob.put", options); err != nil {
		return err
	}
	if options.ContentType == "" {
		options.ContentType = "application/octet-stream"
	}
	metadata := make(map[string]string, len(options.Metadata))
	for name, value := range options.Metadata {
		metadata[name] = value
	}
	sum := sha256.Sum256(body)
	checksum := hex.EncodeToString(sum[:])
	if options.ChecksumSHA256 != "" {
		if err := validateSHA256("blob.put", options.ChecksumSHA256); err != nil {
			return err
		}
		if options.ChecksumSHA256 != checksum {
			return forgeError(CodePrecondition, "blob.put", "blob SHA-256 checksum does not match")
		}
	}
	entry := memoryBlob{
		body:         append([]byte(nil), body...),
		contentType:  options.ContentType,
		etag:         base64.RawURLEncoding.EncodeToString(sum[:]),
		lastModified: f.now(),
		metadata:     metadata,
		cacheControl: options.CacheControl,
		disposition:  options.ContentDisposition,
		checksum:     checksum,
	}
	if f.s3Blob != nil {
		s3Options := options
		s3Options.Metadata = metadata
		s3Options.Metadata[s3ChecksumMetadataKey] = checksum
		return f.s3Blob.put(ctx, key, bytes.NewReader(body), int64(len(body)), s3Options)
	}
	if f.mode == ModePostgres {
		if options.S3Encryption != nil {
			return forgeError(CodeNotConfigured, "blob.put", "S3 encryption requires the S3 blob backend")
		}
		return f.pgBlobPut(ctx, key, entry, options)
	}
	if options.S3Encryption != nil {
		return forgeError(CodeNotConfigured, "blob.put", "S3 encryption requires the S3 blob backend")
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	scoped := f.scoped(key)
	current, exists := f.store.blobs[scoped]
	if options.Precondition != nil && options.Precondition.createOnly && exists {
		return forgeError(CodePrecondition, "blob.put", "object already exists")
	}
	if options.Precondition != nil && options.Precondition.version != "" && (!exists || current.etag != options.Precondition.version) {
		return forgeError(CodePrecondition, "blob.put", "object version does not match")
	}
	f.store.blobs[scoped] = entry
	return nil
}

func (f *Forge) BlobPutStream(ctx context.Context, key string, reader io.Reader, options PutOptions) error {
	if err := f.ready(ctx, "blob.put_stream"); err != nil {
		return err
	}
	if f.s3Blob != nil {
		if err := validateBlobKey("blob.put_stream", key); err != nil {
			return err
		}
		if err := validatePutOptions("blob.put_stream", options); err != nil {
			return err
		}
		if options.ChecksumSHA256 != "" {
			return forgeError(CodeInvalid, "blob.put_stream", "verify a completed stream with BlobVerifyChecksumSHA256")
		}
		return f.s3Blob.putUnknownLength(ctx, key, reader, options)
	}
	limited := io.LimitReader(reader, MaxBufferedBlobBytes+1)
	body, err := io.ReadAll(limited)
	if err != nil {
		return errorWithCause(CodeBackend, "blob.put_stream", "memory", "could not read the upload stream", err)
	}
	return f.BlobPut(ctx, key, body, options)
}

func (f *Forge) BlobGet(ctx context.Context, key string) ([]byte, error) {
	if err := f.ready(ctx, "blob.get"); err != nil {
		return nil, err
	}
	if err := validateBlobKey("blob.get", key); err != nil {
		return nil, err
	}
	if f.s3Blob != nil {
		return f.s3Blob.get(ctx, key)
	}
	if f.mode == ModePostgres {
		return f.pgBlobGet(ctx, key)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	entry, ok := f.store.blobs[f.scoped(key)]
	if !ok {
		return nil, nil
	}
	return append([]byte(nil), entry.body...), nil
}

func (f *Forge) BlobGetIf(ctx context.Context, key string, ifMatch, ifNoneMatch *string) (ConditionalBlobGet, error) {
	if err := f.ready(ctx, "blob.get_if"); err != nil {
		return ConditionalBlobGet{}, err
	}
	if err := validateBlobKey("blob.get_if", key); err != nil {
		return ConditionalBlobGet{}, err
	}
	if ifMatch != nil && ifNoneMatch != nil {
		return ConditionalBlobGet{}, forgeError(CodeInvalid, "blob.get_if", "if_match and if_none_match are mutually exclusive")
	}
	if (ifMatch != nil && *ifMatch == "") || (ifNoneMatch != nil && *ifNoneMatch == "") {
		return ConditionalBlobGet{}, forgeError(CodeInvalid, "blob.get_if", "ETag condition must not be empty")
	}
	if f.s3Blob != nil {
		return f.s3Blob.getIf(ctx, key, ifMatch, ifNoneMatch)
	}
	if f.mode == ModePostgres {
		return f.pgBlobGetIf(ctx, key, ifMatch, ifNoneMatch)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	entry, ok := f.store.blobs[f.scoped(key)]
	if !ok {
		return ConditionalBlobGet{State: "missing"}, nil
	}
	if ifMatch != nil && *ifMatch != entry.etag {
		return ConditionalBlobGet{}, forgeError(CodePrecondition, "blob.get_if", "blob read version does not match")
	}
	if ifNoneMatch != nil && *ifNoneMatch == entry.etag {
		etag := entry.etag
		return ConditionalBlobGet{State: "not_modified", ETag: &etag}, nil
	}
	body := append([]byte(nil), entry.body...)
	etag := entry.etag
	return ConditionalBlobGet{State: "found", Body: &body, ETag: &etag}, nil
}

func (f *Forge) BlobGetRange(ctx context.Context, key string, start, end int64) ([]byte, error) {
	if err := f.ready(ctx, "blob.get_range"); err != nil {
		return nil, err
	}
	if err := validateBlobKey("blob.get_range", key); err != nil {
		return nil, err
	}
	if start < 0 || end < start {
		return nil, forgeError(CodeInvalid, "blob.get_range", "range must be non-negative and ordered")
	}
	if f.s3Blob != nil {
		return f.s3Blob.getRange(ctx, key, start, end)
	}
	body, err := f.BlobGet(ctx, key)
	if err != nil || body == nil {
		return body, err
	}
	if start >= int64(len(body)) {
		return nil, forgeError(CodePrecondition, "blob.get_range", "range starts beyond the object")
	}
	if end >= int64(len(body)) {
		end = int64(len(body)) - 1
	}
	return append([]byte(nil), body[start:end+1]...), nil
}

func (f *Forge) BlobOpen(ctx context.Context, key string) (io.ReadCloser, error) {
	if err := f.ready(ctx, "blob.open"); err != nil {
		return nil, err
	}
	if f.s3Blob != nil {
		return f.s3Blob.open(ctx, key)
	}
	body, err := f.BlobGet(ctx, key)
	if err != nil || body == nil {
		return nil, err
	}
	return io.NopCloser(bytes.NewReader(body)), nil
}

func (f *Forge) BlobHead(ctx context.Context, key string) (*BlobInfo, error) {
	if err := f.ready(ctx, "blob.head"); err != nil {
		return nil, err
	}
	if err := validateBlobKey("blob.head", key); err != nil {
		return nil, err
	}
	if f.s3Blob != nil {
		return f.s3Blob.head(ctx, key)
	}
	if f.mode == ModePostgres {
		return f.pgBlobHead(ctx, key)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	entry, ok := f.store.blobs[f.scoped(key)]
	if !ok {
		return nil, nil
	}
	return blobInfo(key, entry), nil
}

func (f *Forge) BlobDelete(ctx context.Context, key string) error {
	if err := f.ready(ctx, "blob.delete"); err != nil {
		return err
	}
	if err := validateBlobKey("blob.delete", key); err != nil {
		return err
	}
	if f.s3Blob != nil {
		return f.s3Blob.delete(ctx, key)
	}
	if f.mode == ModePostgres {
		return f.pgBlobDelete(ctx, key)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	scoped := f.scoped(key)
	delete(f.store.blobs, scoped)
	return nil
}

func (f *Forge) BlobCopy(ctx context.Context, source, destination string, options PutOptions) (BlobInfo, error) {
	info, err := f.BlobHead(ctx, source)
	if err != nil {
		return BlobInfo{}, err
	}
	if info == nil {
		return BlobInfo{}, forgeError(CodeNotFound, "blob.copy", "source blob does not exist")
	}
	reader, err := f.BlobOpen(ctx, source)
	if err != nil {
		return BlobInfo{}, err
	}
	if reader == nil {
		return BlobInfo{}, forgeError(CodeNotFound, "blob.copy", "source blob does not exist")
	}
	defer reader.Close()
	if options.ContentType == "" {
		options.ContentType = info.ContentType
	}
	if len(options.Metadata) == 0 {
		options.Metadata = info.Metadata
	}
	if options.CacheControl == "" && info.CacheControl != nil {
		options.CacheControl = *info.CacheControl
	}
	if options.ContentDisposition == "" && info.ContentDisposition != nil {
		options.ContentDisposition = *info.ContentDisposition
	}
	if err := f.BlobPutStream(ctx, destination, reader, options); err != nil {
		return BlobInfo{}, err
	}
	copied, err := f.BlobHead(ctx, destination)
	if err != nil {
		return BlobInfo{}, err
	}
	if copied == nil {
		return BlobInfo{}, forgeError(CodeBackend, "blob.copy", "copied blob is not readable")
	}
	return *copied, nil
}

func (f *Forge) BlobCreateMultipart(ctx context.Context, key string, options PutOptions) (MultipartUpload, error) {
	if err := f.ready(ctx, "blob.create_multipart"); err != nil {
		return MultipartUpload{}, err
	}
	if f.s3Blob == nil {
		return MultipartUpload{}, forgeError(CodeNotConfigured, "blob.create_multipart", "multipart handles require the S3 blob backend")
	}
	if err := validatePutOptions("blob.create_multipart", options); err != nil {
		return MultipartUpload{}, err
	}
	return f.s3Blob.createMultipart(ctx, key, options)
}

func (f *Forge) BlobUploadPart(ctx context.Context, upload MultipartUpload, partNumber uint32, body []byte) (MultipartPart, error) {
	if err := f.ready(ctx, "blob.upload_part"); err != nil {
		return MultipartPart{}, err
	}
	if f.s3Blob == nil {
		return MultipartPart{}, forgeError(CodeNotConfigured, "blob.upload_part", "multipart handles require the S3 blob backend")
	}
	return f.s3Blob.uploadPart(ctx, upload, partNumber, body)
}

func (f *Forge) BlobCompleteMultipart(ctx context.Context, upload MultipartUpload, parts []MultipartPart) (BlobInfo, error) {
	if err := f.ready(ctx, "blob.complete_multipart"); err != nil {
		return BlobInfo{}, err
	}
	if f.s3Blob == nil {
		return BlobInfo{}, forgeError(CodeNotConfigured, "blob.complete_multipart", "multipart handles require the S3 blob backend")
	}
	return f.s3Blob.completeMultipart(ctx, upload, parts)
}

func (f *Forge) BlobAbortMultipart(ctx context.Context, upload MultipartUpload) error {
	if err := f.ready(ctx, "blob.abort_multipart"); err != nil {
		return err
	}
	if f.s3Blob == nil {
		return forgeError(CodeNotConfigured, "blob.abort_multipart", "multipart handles require the S3 blob backend")
	}
	return f.s3Blob.abortMultipart(ctx, upload)
}

func (f *Forge) BlobVerifyChecksumSHA256(ctx context.Context, key, expectedHex string) (bool, error) {
	if err := validateSHA256("blob.verify_checksum_sha256", expectedHex); err != nil {
		return false, err
	}
	reader, err := f.BlobOpen(ctx, key)
	if err != nil {
		return false, err
	}
	if reader == nil {
		return false, forgeError(CodeNotFound, "blob.verify_checksum_sha256", "blob does not exist")
	}
	defer reader.Close()
	hasher := sha256.New()
	if _, err := io.Copy(hasher, reader); err != nil {
		return false, errorWithCause(CodeBackend, "blob.verify_checksum_sha256", "blob", "could not read blob for checksum", err)
	}
	return hex.EncodeToString(hasher.Sum(nil)) == expectedHex, nil
}

func (f *Forge) BlobList(ctx context.Context, prefix string, cursor *string, limit uint32) (BlobPage, error) {
	if err := f.ready(ctx, "blob.list"); err != nil {
		return BlobPage{}, err
	}
	if limit == 0 {
		return BlobPage{}, forgeError(CodeInvalid, "blob.list", "limit must be positive")
	}
	if f.s3Blob != nil {
		return f.s3Blob.list(ctx, prefix, cursor, limit)
	}
	after, err := decodeCursor(cursor)
	if err != nil {
		return BlobPage{}, forgeError(CodeInvalid, "blob.list", "cursor is malformed")
	}
	if f.mode == ModePostgres {
		return f.pgBlobList(ctx, prefix, after, limit)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	keys := make([]string, 0)
	namespacePrefix := f.scoped(prefix)
	for scoped := range f.store.blobs {
		if strings.HasPrefix(scoped, namespacePrefix) {
			key := strings.TrimPrefix(scoped, f.namespace+"\x00")
			if key > after {
				keys = append(keys, key)
			}
		}
	}
	sort.Strings(keys)
	pageKeys := keys
	var next *string
	if uint32(len(keys)) > limit {
		pageKeys = keys[:limit]
		value := encodeCursor(pageKeys[len(pageKeys)-1])
		next = &value
	}
	items := make([]BlobSummary, 0, len(pageKeys))
	for _, key := range pageKeys {
		entry := f.store.blobs[f.scoped(key)]
		items = append(items, BlobSummary{Key: key, Size: uint64(len(entry.body)), ETag: entry.etag, LastModifiedMs: float64(entry.lastModified.UnixMilli())})
	}
	return BlobPage{Items: items, Cursor: next}, nil
}

func (f *Forge) BlobPresignDownload(ctx context.Context, key string, expires time.Duration) (ProxyPresign, error) {
	return f.proxyPresign(ctx, "GET", key, expires, 0)
}

func (f *Forge) BlobPresignUpload(ctx context.Context, key string, expires time.Duration, maxBytes uint64) (ProxyPresign, error) {
	if maxBytes == 0 || maxBytes > MaxBufferedBlobBytes {
		return ProxyPresign{}, forgeError(CodeInvalid, "blob.presign_upload", "max bytes must be within the buffered object limit")
	}
	return f.proxyPresign(ctx, "PUT", key, expires, maxBytes)
}

func (f *Forge) proxyPresign(ctx context.Context, method, key string, expires time.Duration, maxBytes uint64) (ProxyPresign, error) {
	if err := f.ready(ctx, "blob.presign"); err != nil {
		return ProxyPresign{}, err
	}
	if len(f.secret) == 0 {
		return ProxyPresign{}, forgeError(CodeNotConfigured, "blob.presign", "a signing secret is required")
	}
	if err := validateBlobKey("blob.presign", key); err != nil {
		return ProxyPresign{}, err
	}
	if expires <= 0 || expires > 7*24*time.Hour {
		return ProxyPresign{}, forgeError(CodeInvalid, "blob.presign", "expiry must be between one second and seven days")
	}
	expiry := f.now().Add(expires).Unix()
	signature := f.proxySignature(method, key, expiry, maxBytes)
	query := url.Values{
		"v":         {"1"},
		"ns":        {f.namespace},
		"method":    {method},
		"expires":   {strconv.FormatInt(expiry, 10)},
		"max_bytes": {strconv.FormatUint(maxBytes, 10)},
		"sig":       {signature},
	}
	return ProxyPresign{
		URL:             "/forge/blob/" + url.PathEscape(key) + "?" + query.Encode(),
		Method:          method,
		Key:             key,
		ExpiresEpoch:    expiry,
		MaxBytes:        maxBytes,
		Signature:       signature,
		RequiredHeaders: map[string]string{},
	}, nil
}

func (f *Forge) BlobPresignNativeGet(ctx context.Context, key string, expires time.Duration) (NativePresign, error) {
	if err := f.ready(ctx, "blob.presign_native_get"); err != nil {
		return NativePresign{}, err
	}
	if f.s3Blob == nil {
		return NativePresign{}, forgeError(CodeNotConfigured, "blob.presign_native_get", "native presigning requires the S3 blob backend")
	}
	return f.s3Blob.presignGet(ctx, key, expires)
}

func (f *Forge) BlobPresignNativePut(ctx context.Context, key string, expires time.Duration, options PutOptions) (NativePresign, error) {
	if err := f.ready(ctx, "blob.presign_native_put"); err != nil {
		return NativePresign{}, err
	}
	if f.s3Blob == nil {
		return NativePresign{}, forgeError(CodeNotConfigured, "blob.presign_native_put", "native presigning requires the S3 blob backend")
	}
	if err := validatePutOptions("blob.presign_native_put", options); err != nil {
		return NativePresign{}, err
	}
	return f.s3Blob.presignPut(ctx, key, expires, options)
}

func (f *Forge) BlobVerifyPresigned(ctx context.Context, method, key string, expiresEpoch int64, maxBytes uint64, signature string) (bool, error) {
	if err := f.ready(ctx, "blob.verify_presigned"); err != nil {
		return false, err
	}
	if method != "GET" && method != "PUT" {
		return false, forgeError(CodeInvalid, "blob.verify_presigned", "method must be GET or PUT")
	}
	if len(f.secret) == 0 {
		return false, forgeError(CodeNotConfigured, "blob.verify_presigned", "a signing secret is required")
	}
	if f.now().Unix() > expiresEpoch {
		return false, nil
	}
	expected, err := base64.RawURLEncoding.DecodeString(f.proxySignature(method, key, expiresEpoch, maxBytes))
	if err != nil {
		return false, nil
	}
	actual, err := base64.RawURLEncoding.DecodeString(signature)
	if err != nil {
		return false, nil
	}
	return subtle.ConstantTimeCompare(expected, actual) == 1, nil
}

func (f *Forge) proxySignature(method, key string, expiry int64, maxBytes uint64) string {
	message := fmt.Sprintf("1\n%s\n%s\n%s\n%d\n%d", f.namespace, method, key, expiry, maxBytes)
	return base64.RawURLEncoding.EncodeToString(hmacSHA256(f.secret, []byte(message)))
}

func hmacSHA256(key, message []byte) []byte {
	const blockSize = 64
	if len(key) > blockSize {
		sum := sha256.Sum256(key)
		key = sum[:]
	}
	padded := make([]byte, blockSize)
	copy(padded, key)
	innerPad := make([]byte, blockSize)
	outerPad := make([]byte, blockSize)
	for index, value := range padded {
		innerPad[index] = value ^ 0x36
		outerPad[index] = value ^ 0x5c
	}
	inner := sha256.New()
	_, _ = inner.Write(innerPad)
	_, _ = inner.Write(message)
	outer := sha256.New()
	_, _ = outer.Write(outerPad)
	_, _ = outer.Write(inner.Sum(nil))
	return outer.Sum(nil)
}

func blobInfo(key string, entry memoryBlob) *BlobInfo {
	metadata := make(map[string]string, len(entry.metadata))
	for name, value := range entry.metadata {
		metadata[name] = value
	}
	return &BlobInfo{
		Key:                key,
		Size:               uint64(len(entry.body)),
		ContentType:        entry.contentType,
		ETag:               entry.etag,
		LastModifiedMs:     float64(entry.lastModified.UnixMilli()),
		Metadata:           metadata,
		CacheControl:       optionalString(entry.cacheControl),
		ContentDisposition: optionalString(entry.disposition),
		ChecksumSha256:     optionalString(entry.checksum),
	}
}

func optionalString(value string) *string {
	if value == "" {
		return nil
	}
	return &value
}

func validateSHA256(operation, value string) error {
	decoded, err := hex.DecodeString(value)
	if err != nil || len(decoded) != sha256.Size || strings.ToLower(value) != value {
		return forgeError(CodeInvalid, operation, "SHA-256 checksum must be 64 lowercase hexadecimal characters")
	}
	return nil
}

func validatePutOptions(operation string, options PutOptions) error {
	if len(options.ContentType) > 256 {
		return forgeError(CodeLimit, operation, "content type exceeds 256 bytes")
	}
	metadataSize := 0
	for name, value := range options.Metadata {
		metadataSize += len(name) + len(value)
	}
	if metadataSize > 2048 {
		return forgeError(CodeLimit, operation, "metadata exceeds 2 KiB")
	}
	if len(options.CacheControl) > 1024 || len(options.ContentDisposition) > 1024 {
		return forgeError(CodeLimit, operation, "HTTP metadata exceeds 1024 bytes")
	}
	if options.ChecksumSHA256 != "" {
		if err := validateSHA256(operation, options.ChecksumSHA256); err != nil {
			return err
		}
	}
	if options.S3Encryption != nil {
		switch options.S3Encryption.Algorithm {
		case "AES256":
			if options.S3Encryption.KMSKeyID != "" {
				return forgeError(CodeInvalid, operation, "an S3-managed encryption request cannot include a KMS key ID")
			}
		case "aws:kms":
			if options.S3Encryption.KMSKeyID == "" {
				return forgeError(CodeInvalid, operation, "KMS-managed encryption requires a provider key ID")
			}
		default:
			return forgeError(CodeInvalid, operation, "S3 encryption must be AES256 or aws:kms")
		}
	}
	if options.Precondition != nil && !options.Precondition.createOnly && options.Precondition.version == "" {
		return forgeError(CodeInvalid, operation, "match-version precondition requires an ETag")
	}
	return nil
}

func validateBlobKey(operation, key string) error {
	if key == "" {
		return forgeError(CodeInvalid, operation, "blob key cannot be empty")
	}
	if len(key) > 1024 {
		return forgeError(CodeLimit, operation, "blob key exceeds 1024 bytes")
	}
	return nil
}
