//go:build s3test

package forge

import (
	"bytes"
	"context"
	"fmt"
	"io"
	"net/http"
	"os"
	"strings"
	"testing"
	"time"
)

type failingReader struct{}

func (failingReader) Read([]byte) (int, error) { return 0, io.ErrClosedPipe }

func testS3Config() S3Config {
	region := os.Getenv("S3_TEST_REGION")
	if region == "" {
		region = "us-east-1"
	}
	return S3Config{
		Bucket: os.Getenv("S3_TEST_BUCKET"), Region: region,
		Endpoint: os.Getenv("S3_TEST_ENDPOINT"), AccessKey: os.Getenv("S3_TEST_ACCESS_KEY"),
		SecretKey: os.Getenv("S3_TEST_SECRET_KEY"), PathStyle: true,
		ConnectTimeout: time.Second, RequestTimeout: 5 * time.Second, MaxRetries: 2,
	}
}

func TestS3CRUDPaginationStreamingAndPresign(t *testing.T) {
	ctx := context.Background()
	blob, err := newS3Blob(ctx, testS3Config(), fmt.Sprintf("go-s3-test-%d", time.Now().UnixNano()))
	if err != nil {
		t.Fatal(err)
	}
	key := "unicode/හෙලෝ-東京.txt"
	if err := blob.put(ctx, key, bytes.NewReader([]byte("hello world")), 11, PutOptions{ContentType: "text/plain", Metadata: map[string]string{"owner": "alice"}, Precondition: CreateOnly()}); err != nil {
		t.Fatal(err)
	}
	if err := blob.put(ctx, key, bytes.NewReader([]byte("duplicate")), 9, PutOptions{Precondition: CreateOnly()}); ErrorCodeOf(err) != CodePrecondition {
		t.Fatalf("create-only code = %s, want PRECONDITION", ErrorCodeOf(err))
	}
	info, err := blob.head(ctx, key)
	if err != nil || info == nil || info.Metadata["owner"] != "alice" {
		t.Fatalf("head = %#v, %v", info, err)
	}
	if err := blob.put(ctx, key, bytes.NewReader([]byte("hello again")), 11, PutOptions{Precondition: MatchVersion(info.ETag)}); err != nil {
		t.Fatal(err)
	}
	rangeBody, err := blob.getRange(ctx, key, 6, 10)
	if err != nil || string(rangeBody) != "again" {
		t.Fatalf("range = %q, %v", rangeBody, err)
	}
	for _, suffix := range []string{"a", "b", "c"} {
		if err := blob.put(ctx, "page/"+suffix, bytes.NewReader([]byte("x")), 1, PutOptions{}); err != nil {
			t.Fatal(err)
		}
	}
	first, err := blob.list(ctx, "page/", nil, 2)
	if err != nil || len(first.Items) != 2 || first.Cursor == nil {
		t.Fatalf("first page = %#v, %v", first, err)
	}
	second, err := blob.list(ctx, "page/", first.Cursor, 2)
	if err != nil || len(second.Items) != 1 || second.Cursor != nil {
		t.Fatalf("second page = %#v, %v", second, err)
	}
	interrupted := io.MultiReader(bytes.NewReader(make([]byte, 1024*1024)), failingReader{})
	if err := blob.putUnknownLength(ctx, "multipart/interrupted", interrupted, PutOptions{}); err == nil {
		t.Fatal("interrupted multipart upload succeeded")
	}
	missing, err := blob.head(ctx, "multipart/interrupted")
	if err != nil || missing != nil {
		t.Fatalf("interrupted object = %#v, %v", missing, err)
	}
	large := bytes.Repeat([]byte{9}, 9*1024*1024)
	if err := blob.putUnknownLength(ctx, "multipart/complete", bytes.NewReader(large), PutOptions{}); err != nil {
		t.Fatal(err)
	}
	largeInfo, err := blob.head(ctx, "multipart/complete")
	if err != nil || largeInfo == nil || largeInfo.Size != uint64(len(large)) {
		t.Fatalf("large head = %#v, %v", largeInfo, err)
	}

	put, err := blob.presignPut(ctx, "native/go.txt", time.Minute, PutOptions{ContentType: "text/plain"})
	if err != nil {
		t.Fatal(err)
	}
	request, _ := http.NewRequestWithContext(ctx, put.Method, put.URL, bytes.NewReader([]byte("native-go")))
	for name, value := range put.RequiredHeaders {
		request.Header.Set(name, value)
	}
	response, err := http.DefaultClient.Do(request)
	if err != nil || response.StatusCode < 200 || response.StatusCode >= 300 {
		if response != nil {
			response.Body.Close()
		}
		t.Fatalf("presigned PUT status = %#v, %v", response, err)
	}
	response.Body.Close()
	get, err := blob.presignGet(ctx, "native/go.txt", time.Minute)
	if err != nil {
		t.Fatal(err)
	}
	request, _ = http.NewRequestWithContext(ctx, get.Method, get.URL, nil)
	for name, value := range get.RequiredHeaders {
		request.Header.Set(name, value)
	}
	response, err = http.DefaultClient.Do(request)
	if err != nil {
		t.Fatal(err)
	}
	body, _ := io.ReadAll(response.Body)
	response.Body.Close()
	if string(body) != "native-go" {
		t.Fatalf("presigned GET = %q", body)
	}

	if err := blob.delete(ctx, key); err != nil {
		t.Fatal(err)
	}
	if err := blob.delete(ctx, key); err != nil {
		t.Fatal(err)
	}
}

func TestS3ConditionalReadsHeadersChecksumsEncryptionAndMultipartHandles(t *testing.T) {
	ctx := context.Background()
	blob, err := newS3Blob(ctx, testS3Config(), fmt.Sprintf("go-s3-ergonomics-%d", time.Now().UnixNano()))
	if err != nil {
		t.Fatal(err)
	}
	const checksum = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
	options := PutOptions{
		ContentType:        "text/plain",
		Metadata:           map[string]string{"owner": "alice", s3ChecksumMetadataKey: checksum},
		CacheControl:       "public, max-age=60",
		ContentDisposition: "attachment; filename=source.txt",
		ChecksumSHA256:     checksum,
	}
	if err := blob.put(ctx, "source.txt", bytes.NewReader([]byte("hello world")), 11, options); err != nil {
		t.Fatal(err)
	}
	info, err := blob.head(ctx, "source.txt")
	if err != nil || info == nil || info.ChecksumSha256 == nil || *info.ChecksumSha256 != checksum {
		t.Fatalf("head = %#v, %v", info, err)
	}
	found, err := blob.getIf(ctx, "source.txt", &info.ETag, nil)
	if err != nil || found.State != "found" || found.Body == nil || string(*found.Body) != "hello world" {
		t.Fatalf("conditional read = %#v, %v", found, err)
	}
	notModified, err := blob.getIf(ctx, "source.txt", nil, &info.ETag)
	if err != nil || notModified.State != "not_modified" || notModified.Body != nil {
		t.Fatalf("not-modified read = %#v, %v", notModified, err)
	}

	upload, err := blob.createMultipart(ctx, "multipart.bin", PutOptions{CacheControl: "private, max-age=0"})
	if err != nil {
		t.Fatal(err)
	}
	first, err := blob.uploadPart(ctx, upload, 1, bytes.Repeat([]byte{7}, 5*1024*1024))
	if err != nil {
		t.Fatal(err)
	}
	second, err := blob.uploadPart(ctx, upload, 2, []byte("tail"))
	if err != nil {
		t.Fatal(err)
	}
	completed, err := blob.completeMultipart(ctx, upload, []MultipartPart{first, second})
	if err != nil || completed.Size != 5*1024*1024+4 || completed.CacheControl == nil || *completed.CacheControl != "private, max-age=0" {
		t.Fatalf("multipart completion = %#v, %v", completed, err)
	}
	abandoned, err := blob.createMultipart(ctx, "abandoned.bin", PutOptions{})
	if err != nil {
		t.Fatal(err)
	}
	if err := blob.abortMultipart(ctx, abandoned); err != nil {
		t.Fatal(err)
	}
	if err := blob.abortMultipart(ctx, abandoned); err != nil {
		t.Fatal(err)
	}

	presign, err := blob.presignPut(ctx, "encrypted.txt", time.Minute, PutOptions{S3Encryption: S3ManagedEncryption()})
	if err != nil {
		t.Fatal(err)
	}
	foundEncryption := false
	for name, value := range presign.RequiredHeaders {
		if strings.EqualFold(name, "x-amz-server-side-encryption") && value == "AES256" {
			foundEncryption = true
		}
	}
	if !foundEncryption {
		t.Fatalf("S3 encryption header was not signed: %#v", presign.RequiredHeaders)
	}
}

func TestS3ProbeRejectsBadCredentialsAndMissingBucket(t *testing.T) {
	ctx := context.Background()
	bad := testS3Config()
	bad.AccessKey, bad.SecretKey = "expired", "expired"
	if _, err := newS3Blob(ctx, bad, "go-bad-creds"); ErrorCodeOf(err) != CodeConfig {
		t.Fatalf("bad credentials code = %s, want CONFIG", ErrorCodeOf(err))
	}
	expired := testS3Config()
	expired.SessionToken = "expired-session-token"
	if _, err := newS3Blob(ctx, expired, "go-expired-session"); ErrorCodeOf(err) != CodeConfig {
		t.Fatalf("expired session code = %s, want CONFIG", ErrorCodeOf(err))
	}
	missing := testS3Config()
	missing.Bucket = "forge-missing-bucket"
	if _, err := newS3Blob(ctx, missing, "go-missing"); ErrorCodeOf(err) != CodeConfig {
		t.Fatalf("missing bucket code = %s, want CONFIG", ErrorCodeOf(err))
	}
	denied := testS3Config()
	denied.AccessKey = os.Getenv("S3_TEST_DENIED_ACCESS_KEY")
	denied.SecretKey = os.Getenv("S3_TEST_DENIED_SECRET_KEY")
	if _, err := newS3Blob(ctx, denied, "go-denied"); ErrorCodeOf(err) != CodeConfig {
		t.Fatalf("denied permissions code = %s, want CONFIG", ErrorCodeOf(err))
	}
}

func TestPublicForgeS3PackageBoundary(t *testing.T) {
	ctx := context.Background()
	namespace := fmt.Sprintf("go-s3-public-%d", time.Now().UnixNano())
	s3Config := testS3Config()
	s3Config.Prefix = "public-boundary"
	forge, err := Init(ctx, Config{
		Mode:          ModePostgres,
		Environment:   EnvironmentTest,
		Namespace:     namespace,
		PostgresURL:   os.Getenv("TEST_DATABASE_URL"),
		AutoMigrate:   true,
		BlobBackend:   "s3",
		S3:            &s3Config,
		SigningSecret: []byte("public-boundary-secret"),
	})
	if err != nil {
		t.Fatal(err)
	}
	defer forge.Close(context.Background())

	if err := forge.BlobPutStream(ctx, "streamed.txt", strings.NewReader("streamed"), PutOptions{ContentType: "text/plain"}); err != nil {
		t.Fatal(err)
	}
	upload, err := forge.BlobCreateMultipart(ctx, "handled.bin", PutOptions{ContentType: "application/octet-stream"})
	if err != nil {
		t.Fatal(err)
	}
	first, err := forge.BlobUploadPart(ctx, upload, 1, bytes.Repeat([]byte{3}, 5*1024*1024))
	if err != nil {
		t.Fatal(err)
	}
	second, err := forge.BlobUploadPart(ctx, upload, 2, []byte("tail"))
	if err != nil {
		t.Fatal(err)
	}
	completed, err := forge.BlobCompleteMultipart(ctx, upload, []MultipartPart{first, second})
	if err != nil || completed.Size != 5*1024*1024+4 {
		t.Fatalf("public multipart completion failed: info=%+v err=%v", completed, err)
	}
	abandoned, err := forge.BlobCreateMultipart(ctx, "abandoned.bin", PutOptions{})
	if err != nil {
		t.Fatal(err)
	}
	if err := forge.BlobAbortMultipart(ctx, abandoned); err != nil {
		t.Fatal(err)
	}
	if _, err := forge.BlobPresignNativePut(ctx, "native.txt", time.Minute, PutOptions{ContentType: "text/plain"}); err != nil {
		t.Fatal(err)
	}
	if _, err := forge.BlobPresignNativeGet(ctx, "handled.bin", time.Minute); err != nil {
		t.Fatal(err)
	}
}
