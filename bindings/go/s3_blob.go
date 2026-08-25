package forge

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"strings"
	"time"

	"github.com/aws/aws-sdk-go-v2/aws"
	awsconfig "github.com/aws/aws-sdk-go-v2/config"
	"github.com/aws/aws-sdk-go-v2/credentials"
	"github.com/aws/aws-sdk-go-v2/service/s3"
	"github.com/aws/aws-sdk-go-v2/service/s3/types"
	smithyhttp "github.com/aws/smithy-go/transport/http"
)

const s3PartBytes = 8 * 1024 * 1024
const s3ChecksumMetadataKey = "forge-checksum-sha256"

// S3Config configures standard AWS S3 or an S3-compatible endpoint.
type S3Config struct {
	Bucket         string
	Region         string
	Endpoint       string
	Prefix         string
	AccessKey      string
	SecretKey      string
	SessionToken   string
	PathStyle      bool
	ConnectTimeout time.Duration
	RequestTimeout time.Duration
	MaxRetries     uint32
}

type s3Blob struct {
	client  *s3.Client
	presign *s3.PresignClient
	bucket  string
	prefix  string
}

func newS3Blob(ctx context.Context, config S3Config, namespace string) (*s3Blob, error) {
	if strings.TrimSpace(config.Bucket) == "" {
		return nil, forgeError(CodeConfig, "blob.s3", "S3 bucket is required")
	}
	if (config.AccessKey == "") != (config.SecretKey == "") {
		return nil, forgeError(CodeConfig, "blob.s3", "S3 access key and secret key must be configured together")
	}
	if config.Region == "" {
		config.Region = "us-east-1"
	}
	if config.ConnectTimeout == 0 {
		config.ConnectTimeout = 3 * time.Second
	}
	if config.RequestTimeout == 0 {
		config.RequestTimeout = 30 * time.Second
	}
	if config.MaxRetries == 0 {
		config.MaxRetries = 3
	}
	if config.MaxRetries > 10 {
		return nil, forgeError(CodeConfig, "blob.s3", "S3 max retries must not exceed 10")
	}
	transport := http.DefaultTransport.(*http.Transport).Clone()
	transport.DialContext = (&net.Dialer{Timeout: config.ConnectTimeout, KeepAlive: 30 * time.Second}).DialContext
	options := []func(*awsconfig.LoadOptions) error{
		awsconfig.WithRegion(config.Region),
		awsconfig.WithRetryMaxAttempts(int(config.MaxRetries) + 1),
		awsconfig.WithHTTPClient(&http.Client{Transport: transport, Timeout: config.RequestTimeout}),
	}
	if config.AccessKey != "" {
		options = append(options, awsconfig.WithCredentialsProvider(credentials.NewStaticCredentialsProvider(
			config.AccessKey, config.SecretKey, config.SessionToken,
		)))
	}
	loaded, err := awsconfig.LoadDefaultConfig(ctx, options...)
	if err != nil {
		return nil, errorWithCause(CodeConfig, "blob.s3", "s3", "could not load S3 configuration", err)
	}
	client := s3.NewFromConfig(loaded, func(options *s3.Options) {
		options.UsePathStyle = config.PathStyle
		if config.Endpoint != "" {
			options.BaseEndpoint = aws.String(config.Endpoint)
		}
	})
	prefix := strings.Trim(strings.Join([]string{strings.Trim(config.Prefix, "/"), namespace}, "/"), "/")
	blob := &s3Blob{client: client, presign: s3.NewPresignClient(client), bucket: config.Bucket, prefix: prefix}
	if err := blob.probe(ctx); err != nil {
		return nil, err
	}
	return blob, nil
}

func (b *s3Blob) physical(key string) string {
	if b.prefix == "" {
		return key
	}
	if key == "" {
		return b.prefix + "/"
	}
	return b.prefix + "/" + key
}

func (b *s3Blob) logical(key string) string {
	if b.prefix == "" {
		return key
	}
	return strings.TrimPrefix(strings.TrimPrefix(key, b.prefix), "/")
}

func (b *s3Blob) probe(ctx context.Context) error {
	if _, err := b.client.HeadBucket(ctx, &s3.HeadBucketInput{Bucket: aws.String(b.bucket)}); err != nil {
		return errorWithCause(CodeConfig, "blob.s3.probe", "s3", "S3 credentials cannot access the configured bucket", err)
	}
	key := b.physical(fmt.Sprintf(".forge-probe/%d", time.Now().UnixNano()))
	created, err := b.client.CreateMultipartUpload(ctx, &s3.CreateMultipartUploadInput{Bucket: aws.String(b.bucket), Key: aws.String(key)})
	if err != nil {
		return errorWithCause(CodeConfig, "blob.s3.probe", "s3", "S3 credentials cannot initiate uploads", err)
	}
	_, err = b.client.AbortMultipartUpload(ctx, &s3.AbortMultipartUploadInput{Bucket: aws.String(b.bucket), Key: aws.String(key), UploadId: created.UploadId})
	if err != nil {
		return errorWithCause(CodeConfig, "blob.s3.probe", "s3", "S3 credentials cannot abort uploads", err)
	}
	return nil
}

func s3Status(err error) int {
	var response *smithyhttp.ResponseError
	if errors.As(err, &response) {
		return response.HTTPStatusCode()
	}
	return 0
}

func s3OperationError(operation string, err error) error {
	status := s3Status(err)
	if status == http.StatusPreconditionFailed || status == http.StatusConflict {
		return errorWithCause(CodePrecondition, operation, "s3", "S3 write precondition failed", err)
	}
	if status == http.StatusTooManyRequests || status >= 500 {
		return errorWithCause(CodeUnavailable, operation, "s3", "S3 is temporarily unavailable", err)
	}
	return errorWithCause(CodeBackend, operation, "s3", "S3 operation failed", err)
}

func applyPutOptions(input *s3.PutObjectInput, options PutOptions) {
	if options.ContentType == "" {
		options.ContentType = "application/octet-stream"
	}
	input.ContentType = aws.String(options.ContentType)
	input.Metadata = options.Metadata
	input.CacheControl = optionalString(options.CacheControl)
	input.ContentDisposition = optionalString(options.ContentDisposition)
	if options.ChecksumSHA256 != "" {
		if decoded, err := hex.DecodeString(options.ChecksumSHA256); err == nil {
			input.ChecksumSHA256 = aws.String(base64.StdEncoding.EncodeToString(decoded))
		}
	}
	if options.S3Encryption != nil {
		input.ServerSideEncryption = types.ServerSideEncryption(options.S3Encryption.Algorithm)
		input.SSEKMSKeyId = optionalString(options.S3Encryption.KMSKeyID)
	}
	if options.Precondition != nil {
		if options.Precondition.createOnly {
			input.IfNoneMatch = aws.String("*")
		} else if options.Precondition.version != "" {
			input.IfMatch = aws.String(options.Precondition.version)
		}
	}
}

func (b *s3Blob) put(ctx context.Context, key string, reader io.Reader, length int64, options PutOptions) error {
	input := &s3.PutObjectInput{Bucket: aws.String(b.bucket), Key: aws.String(b.physical(key)), Body: reader, ContentLength: aws.Int64(length)}
	applyPutOptions(input, options)
	_, err := b.client.PutObject(ctx, input)
	if err != nil {
		return s3OperationError("blob.put", err)
	}
	return nil
}

func (b *s3Blob) putUnknownLength(ctx context.Context, key string, reader io.Reader, options PutOptions) error {
	if err := validateBlobKey("blob.put_stream", key); err != nil {
		return err
	}
	contentType := options.ContentType
	if contentType == "" {
		contentType = "application/octet-stream"
	}
	createInput := &s3.CreateMultipartUploadInput{
		Bucket: aws.String(b.bucket), Key: aws.String(b.physical(key)), ContentType: aws.String(contentType), Metadata: options.Metadata,
	}
	applyMultipartOptions(createInput, options)
	created, err := b.client.CreateMultipartUpload(ctx, createInput)
	if err != nil {
		return s3OperationError("blob.put_stream", err)
	}
	abort := func() {
		_, _ = b.client.AbortMultipartUpload(context.WithoutCancel(ctx), &s3.AbortMultipartUploadInput{
			Bucket: aws.String(b.bucket), Key: aws.String(b.physical(key)), UploadId: created.UploadId,
		})
	}
	parts := make([]types.CompletedPart, 0)
	for partNumber := int32(1); ; partNumber++ {
		body := make([]byte, s3PartBytes)
		n, readErr := io.ReadFull(reader, body)
		if readErr == io.EOF {
			if len(parts) == 0 {
				abort()
				return b.put(ctx, key, bytes.NewReader(nil), 0, options)
			}
			break
		}
		if readErr != nil && readErr != io.ErrUnexpectedEOF {
			abort()
			return errorWithCause(CodeBackend, "blob.put_stream", "s3", "could not read the upload stream", readErr)
		}
		body = body[:n]
		uploaded, uploadErr := b.client.UploadPart(ctx, &s3.UploadPartInput{
			Bucket: aws.String(b.bucket), Key: aws.String(b.physical(key)), UploadId: created.UploadId,
			PartNumber: aws.Int32(partNumber), ContentLength: aws.Int64(int64(n)), Body: bytes.NewReader(body),
		})
		if uploadErr != nil {
			abort()
			return s3OperationError("blob.put_stream", uploadErr)
		}
		parts = append(parts, types.CompletedPart{ETag: uploaded.ETag, PartNumber: aws.Int32(partNumber)})
		if readErr == io.ErrUnexpectedEOF {
			break
		}
	}
	complete := &s3.CompleteMultipartUploadInput{
		Bucket: aws.String(b.bucket), Key: aws.String(b.physical(key)), UploadId: created.UploadId,
		MultipartUpload: &types.CompletedMultipartUpload{Parts: parts},
	}
	if options.Precondition != nil {
		if options.Precondition.createOnly {
			complete.IfNoneMatch = aws.String("*")
		} else if options.Precondition.version != "" {
			complete.IfMatch = aws.String(options.Precondition.version)
		}
	}
	if _, err := b.client.CompleteMultipartUpload(ctx, complete); err != nil {
		abort()
		return s3OperationError("blob.put_stream", err)
	}
	return nil
}

func (b *s3Blob) get(ctx context.Context, key string) ([]byte, error) {
	output, err := b.client.GetObject(ctx, &s3.GetObjectInput{Bucket: aws.String(b.bucket), Key: aws.String(b.physical(key))})
	if err != nil {
		if s3Status(err) == http.StatusNotFound {
			return nil, nil
		}
		return nil, s3OperationError("blob.get", err)
	}
	defer output.Body.Close()
	if output.ContentLength != nil && *output.ContentLength > MaxBufferedBlobBytes {
		return nil, forgeError(CodeLimit, "blob.get", "object exceeds the 50 MiB buffered limit; use BlobOpen")
	}
	body, err := io.ReadAll(io.LimitReader(output.Body, MaxBufferedBlobBytes+1))
	if err != nil {
		return nil, errorWithCause(CodeBackend, "blob.get", "s3", "could not read S3 response", err)
	}
	if len(body) > MaxBufferedBlobBytes {
		return nil, forgeError(CodeLimit, "blob.get", "object exceeds the 50 MiB buffered limit; use BlobOpen")
	}
	return body, nil
}

func (b *s3Blob) getIf(ctx context.Context, key string, ifMatch, ifNoneMatch *string) (ConditionalBlobGet, error) {
	output, err := b.client.GetObject(ctx, &s3.GetObjectInput{
		Bucket: aws.String(b.bucket), Key: aws.String(b.physical(key)), IfMatch: ifMatch, IfNoneMatch: ifNoneMatch,
	})
	if err != nil {
		switch s3Status(err) {
		case http.StatusNotFound:
			return ConditionalBlobGet{State: "missing"}, nil
		case http.StatusNotModified:
			return ConditionalBlobGet{State: "not_modified", ETag: ifNoneMatch}, nil
		case http.StatusPreconditionFailed:
			return ConditionalBlobGet{}, forgeError(CodePrecondition, "blob.get_if", "blob read version does not match")
		default:
			return ConditionalBlobGet{}, s3OperationError("blob.get_if", err)
		}
	}
	defer output.Body.Close()
	body, err := io.ReadAll(io.LimitReader(output.Body, MaxBufferedBlobBytes+1))
	if err != nil {
		return ConditionalBlobGet{}, errorWithCause(CodeBackend, "blob.get_if", "s3", "could not read S3 response", err)
	}
	if len(body) > MaxBufferedBlobBytes {
		return ConditionalBlobGet{}, forgeError(CodeLimit, "blob.get_if", "object exceeds the 50 MiB buffered limit")
	}
	etag := aws.ToString(output.ETag)
	return ConditionalBlobGet{State: "found", Body: &body, ETag: &etag}, nil
}

func (b *s3Blob) open(ctx context.Context, key string) (io.ReadCloser, error) {
	output, err := b.client.GetObject(ctx, &s3.GetObjectInput{Bucket: aws.String(b.bucket), Key: aws.String(b.physical(key))})
	if err != nil {
		if s3Status(err) == http.StatusNotFound {
			return nil, nil
		}
		return nil, s3OperationError("blob.open", err)
	}
	return output.Body, nil
}

func (b *s3Blob) getRange(ctx context.Context, key string, start, end int64) ([]byte, error) {
	if end-start+1 > MaxBufferedBlobBytes {
		return nil, forgeError(CodeLimit, "blob.get_range", "range exceeds the 50 MiB buffered limit")
	}
	output, err := b.client.GetObject(ctx, &s3.GetObjectInput{
		Bucket: aws.String(b.bucket), Key: aws.String(b.physical(key)), Range: aws.String(fmt.Sprintf("bytes=%d-%d", start, end)),
	})
	if err != nil {
		if s3Status(err) == http.StatusNotFound {
			return nil, nil
		}
		return nil, s3OperationError("blob.get_range", err)
	}
	defer output.Body.Close()
	body, err := io.ReadAll(output.Body)
	if err != nil {
		return nil, errorWithCause(CodeBackend, "blob.get_range", "s3", "could not read S3 range response", err)
	}
	return body, nil
}

func (b *s3Blob) head(ctx context.Context, key string) (*BlobInfo, error) {
	output, err := b.client.HeadObject(ctx, &s3.HeadObjectInput{Bucket: aws.String(b.bucket), Key: aws.String(b.physical(key))})
	if err != nil {
		if s3Status(err) == http.StatusNotFound {
			return nil, nil
		}
		return nil, s3OperationError("blob.head", err)
	}
	metadata := make(map[string]string, len(output.Metadata))
	for name, value := range output.Metadata {
		metadata[name] = value
	}
	checksum := metadata[s3ChecksumMetadataKey]
	delete(metadata, s3ChecksumMetadataKey)
	info := &BlobInfo{
		Key: key, ContentType: aws.ToString(output.ContentType), ETag: aws.ToString(output.ETag), Metadata: metadata,
		CacheControl: output.CacheControl, ContentDisposition: output.ContentDisposition,
		ChecksumSha256: optionalString(checksum),
	}
	if output.ServerSideEncryption != "" {
		value := string(output.ServerSideEncryption)
		info.ServerSideEncryption = &value
	}
	if output.ContentLength != nil {
		info.Size = uint64(*output.ContentLength)
	}
	if output.LastModified != nil {
		info.LastModifiedMs = float64(output.LastModified.UnixMilli())
	}
	return info, nil
}

func applyMultipartOptions(input *s3.CreateMultipartUploadInput, options PutOptions) {
	if options.ContentType == "" {
		options.ContentType = "application/octet-stream"
	}
	input.ContentType = aws.String(options.ContentType)
	input.Metadata = options.Metadata
	input.CacheControl = optionalString(options.CacheControl)
	input.ContentDisposition = optionalString(options.ContentDisposition)
	if options.S3Encryption != nil {
		input.ServerSideEncryption = types.ServerSideEncryption(options.S3Encryption.Algorithm)
		input.SSEKMSKeyId = optionalString(options.S3Encryption.KMSKeyID)
	}
}

func (b *s3Blob) createMultipart(ctx context.Context, key string, options PutOptions) (MultipartUpload, error) {
	if err := validateBlobKey("blob.create_multipart", key); err != nil {
		return MultipartUpload{}, err
	}
	if options.ChecksumSHA256 != "" {
		return MultipartUpload{}, forgeError(CodeInvalid, "blob.create_multipart", "verify a completed upload with BlobVerifyChecksumSHA256")
	}
	input := &s3.CreateMultipartUploadInput{Bucket: aws.String(b.bucket), Key: aws.String(b.physical(key))}
	applyMultipartOptions(input, options)
	created, err := b.client.CreateMultipartUpload(ctx, input)
	if err != nil {
		return MultipartUpload{}, s3OperationError("blob.create_multipart", err)
	}
	handle := MultipartUpload{Key: key, UploadID: aws.ToString(created.UploadId)}
	if options.Precondition != nil {
		handle.CreateOnly = options.Precondition.createOnly
		if options.Precondition.version != "" {
			value := options.Precondition.version
			handle.MatchVersion = &value
		}
	}
	return handle, nil
}

func (b *s3Blob) uploadPart(ctx context.Context, upload MultipartUpload, partNumber uint32, body []byte) (MultipartPart, error) {
	if err := validateBlobKey("blob.upload_part", upload.Key); err != nil {
		return MultipartPart{}, err
	}
	if partNumber == 0 || partNumber > 10000 {
		return MultipartPart{}, forgeError(CodeInvalid, "blob.upload_part", "part number must be 1..=10000")
	}
	if len(body) == 0 || len(body) > MaxBufferedBlobBytes {
		return MultipartPart{}, forgeError(CodeLimit, "blob.upload_part", "part must be between 1 byte and 50 MiB")
	}
	output, err := b.client.UploadPart(ctx, &s3.UploadPartInput{
		Bucket: aws.String(b.bucket), Key: aws.String(b.physical(upload.Key)), UploadId: aws.String(upload.UploadID),
		PartNumber: aws.Int32(int32(partNumber)), ContentLength: aws.Int64(int64(len(body))), Body: bytes.NewReader(body),
	})
	if err != nil {
		return MultipartPart{}, s3OperationError("blob.upload_part", err)
	}
	return MultipartPart{PartNumber: partNumber, ETag: aws.ToString(output.ETag), Size: uint64(len(body))}, nil
}

func (b *s3Blob) completeMultipart(ctx context.Context, upload MultipartUpload, parts []MultipartPart) (BlobInfo, error) {
	if len(parts) == 0 || len(parts) > 10000 {
		return BlobInfo{}, forgeError(CodeInvalid, "blob.complete_multipart", "completion requires 1..=10000 parts")
	}
	completed := make([]types.CompletedPart, 0, len(parts))
	var previous uint32
	for _, part := range parts {
		if part.PartNumber <= previous || part.PartNumber > 10000 {
			return BlobInfo{}, forgeError(CodeInvalid, "blob.complete_multipart", "parts must be strictly ordered")
		}
		previous = part.PartNumber
		completed = append(completed, types.CompletedPart{PartNumber: aws.Int32(int32(part.PartNumber)), ETag: aws.String(part.ETag)})
	}
	input := &s3.CompleteMultipartUploadInput{
		Bucket: aws.String(b.bucket), Key: aws.String(b.physical(upload.Key)), UploadId: aws.String(upload.UploadID),
		MultipartUpload: &types.CompletedMultipartUpload{Parts: completed},
	}
	if upload.CreateOnly {
		input.IfNoneMatch = aws.String("*")
	} else if upload.MatchVersion != nil {
		input.IfMatch = upload.MatchVersion
	}
	if _, err := b.client.CompleteMultipartUpload(ctx, input); err != nil {
		return BlobInfo{}, s3OperationError("blob.complete_multipart", err)
	}
	info, err := b.head(ctx, upload.Key)
	if err != nil {
		return BlobInfo{}, err
	}
	if info == nil {
		return BlobInfo{}, forgeError(CodeBackend, "blob.complete_multipart", "completed blob is not readable")
	}
	return *info, nil
}

func (b *s3Blob) abortMultipart(ctx context.Context, upload MultipartUpload) error {
	if err := validateBlobKey("blob.abort_multipart", upload.Key); err != nil {
		return err
	}
	_, err := b.client.AbortMultipartUpload(ctx, &s3.AbortMultipartUploadInput{
		Bucket: aws.String(b.bucket), Key: aws.String(b.physical(upload.Key)), UploadId: aws.String(upload.UploadID),
	})
	if err != nil && s3Status(err) != http.StatusNotFound {
		return s3OperationError("blob.abort_multipart", err)
	}
	return nil
}

func (b *s3Blob) delete(ctx context.Context, key string) error {
	_, err := b.client.DeleteObject(ctx, &s3.DeleteObjectInput{Bucket: aws.String(b.bucket), Key: aws.String(b.physical(key))})
	if err != nil {
		return s3OperationError("blob.delete", err)
	}
	return nil
}

func (b *s3Blob) list(ctx context.Context, prefix string, cursor *string, limit uint32) (BlobPage, error) {
	output, err := b.client.ListObjectsV2(ctx, &s3.ListObjectsV2Input{
		Bucket: aws.String(b.bucket), Prefix: aws.String(b.physical(prefix)), ContinuationToken: cursor, MaxKeys: aws.Int32(int32(min(limit, 1000))),
	})
	if err != nil {
		return BlobPage{}, s3OperationError("blob.list", err)
	}
	items := make([]BlobSummary, 0, len(output.Contents))
	for _, object := range output.Contents {
		summary := BlobSummary{Key: b.logical(aws.ToString(object.Key)), ETag: aws.ToString(object.ETag)}
		if object.Size != nil {
			summary.Size = uint64(*object.Size)
		}
		if object.LastModified != nil {
			summary.LastModifiedMs = float64(object.LastModified.UnixMilli())
		}
		items = append(items, summary)
	}
	return BlobPage{Items: items, Cursor: output.NextContinuationToken}, nil
}

func requiredHeaders(headers http.Header) map[string]string {
	result := make(map[string]string, len(headers))
	for name, values := range headers {
		result[name] = strings.Join(values, ",")
	}
	return result
}

func validNativeExpiry(expires time.Duration) error {
	if expires <= 0 || expires > 7*24*time.Hour {
		return forgeError(CodeInvalid, "blob.presign_native", "expiry must be between one second and seven days")
	}
	return nil
}

func (b *s3Blob) presignGet(ctx context.Context, key string, expires time.Duration) (NativePresign, error) {
	if err := validateBlobKey("blob.presign_native_get", key); err != nil {
		return NativePresign{}, err
	}
	if err := validNativeExpiry(expires); err != nil {
		return NativePresign{}, err
	}
	request, err := b.presign.PresignGetObject(ctx, &s3.GetObjectInput{Bucket: aws.String(b.bucket), Key: aws.String(b.physical(key))}, s3.WithPresignExpires(expires))
	if err != nil {
		return NativePresign{}, s3OperationError("blob.presign_native_get", err)
	}
	return NativePresign{URL: request.URL, Method: request.Method, ExpiresEpoch: time.Now().Add(expires).Unix(), RequiredHeaders: requiredHeaders(request.SignedHeader), Constraints: map[string]string{"bearer_credential": "true"}}, nil
}

func (b *s3Blob) presignPut(ctx context.Context, key string, expires time.Duration, options PutOptions) (NativePresign, error) {
	if err := validateBlobKey("blob.presign_native_put", key); err != nil {
		return NativePresign{}, err
	}
	if err := validNativeExpiry(expires); err != nil {
		return NativePresign{}, err
	}
	input := &s3.PutObjectInput{Bucket: aws.String(b.bucket), Key: aws.String(b.physical(key))}
	applyPutOptions(input, options)
	request, err := b.presign.PresignPutObject(ctx, input, s3.WithPresignExpires(expires))
	if err != nil {
		return NativePresign{}, s3OperationError("blob.presign_native_put", err)
	}
	return NativePresign{URL: request.URL, Method: request.Method, ExpiresEpoch: time.Now().Add(expires).Unix(), RequiredHeaders: requiredHeaders(request.SignedHeader), Constraints: map[string]string{"bearer_credential": "true", "maximum_body_size": "not_portably_enforced"}}, nil
}
