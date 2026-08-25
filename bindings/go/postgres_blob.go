package forge

import (
	"context"
	"encoding/json"
	"time"

	"github.com/jackc/pgx/v5"
)

func (f *Forge) pgBlobPut(ctx context.Context, key string, entry memoryBlob, options PutOptions) error {
	metadata, err := json.Marshal(entry.metadata)
	if err != nil {
		return forgeError(CodeInvalid, "blob.put", "metadata is not encodable")
	}
	physical := f.pgScoped(key)
	query := "INSERT INTO forge_blobs (key, data, content_type, etag, metadata, size, last_modified, cache_control, content_disposition, checksum_sha256) VALUES ($1, $2, $3, $4, $5::jsonb, $6, $7, $8, $9, $10) ON CONFLICT (key) DO UPDATE SET data = EXCLUDED.data, content_type = EXCLUDED.content_type, etag = EXCLUDED.etag, metadata = EXCLUDED.metadata, size = EXCLUDED.size, last_modified = EXCLUDED.last_modified, cache_control = EXCLUDED.cache_control, content_disposition = EXCLUDED.content_disposition, checksum_sha256 = EXCLUDED.checksum_sha256 RETURNING key"
	args := []any{physical, entry.body, entry.contentType, entry.etag, metadata, len(entry.body), entry.lastModified, nullableString(entry.cacheControl), nullableString(entry.disposition), entry.checksum}
	if options.Precondition != nil && options.Precondition.createOnly {
		query = "INSERT INTO forge_blobs (key, data, content_type, etag, metadata, size, last_modified, cache_control, content_disposition, checksum_sha256) VALUES ($1, $2, $3, $4, $5::jsonb, $6, $7, $8, $9, $10) ON CONFLICT DO NOTHING RETURNING key"
	} else if options.Precondition != nil && options.Precondition.version != "" {
		query = "INSERT INTO forge_blobs (key, data, content_type, etag, metadata, size, last_modified, cache_control, content_disposition, checksum_sha256) VALUES ($1, $2, $3, $4, $5::jsonb, $6, $7, $8, $9, $10) ON CONFLICT (key) DO UPDATE SET data = EXCLUDED.data, content_type = EXCLUDED.content_type, etag = EXCLUDED.etag, metadata = EXCLUDED.metadata, size = EXCLUDED.size, last_modified = EXCLUDED.last_modified, cache_control = EXCLUDED.cache_control, content_disposition = EXCLUDED.content_disposition, checksum_sha256 = EXCLUDED.checksum_sha256 WHERE forge_blobs.etag = $11 RETURNING key"
		args = append(args, options.Precondition.version)
	}
	var stored string
	err = f.postgres(PrimitiveBlob).QueryRow(ctx, query, args...).Scan(&stored)
	if err == pgx.ErrNoRows {
		return forgeError(CodePrecondition, "blob.put", "object write precondition failed")
	}
	return postgresError("blob.put", err)
}

func (f *Forge) pgBlobGetIf(ctx context.Context, key string, ifMatch, ifNoneMatch *string) (ConditionalBlobGet, error) {
	var body []byte
	var etag string
	err := f.postgres(PrimitiveBlob).QueryRow(ctx, "SELECT data, etag FROM forge_blobs WHERE key = $1", f.pgScoped(key)).Scan(&body, &etag)
	if err == pgx.ErrNoRows {
		return ConditionalBlobGet{State: "missing"}, nil
	}
	if err != nil {
		return ConditionalBlobGet{}, postgresError("blob.get_if", err)
	}
	if ifMatch != nil && *ifMatch != etag {
		return ConditionalBlobGet{}, forgeError(CodePrecondition, "blob.get_if", "blob read version does not match")
	}
	if ifNoneMatch != nil && *ifNoneMatch == etag {
		return ConditionalBlobGet{State: "not_modified", ETag: &etag}, nil
	}
	return ConditionalBlobGet{State: "found", Body: &body, ETag: &etag}, nil
}

func (f *Forge) pgBlobGet(ctx context.Context, key string) ([]byte, error) {
	var body []byte
	err := f.postgres(PrimitiveBlob).QueryRow(ctx, "SELECT data FROM forge_blobs WHERE key = $1", f.pgScoped(key)).Scan(&body)
	if err == pgx.ErrNoRows {
		return nil, nil
	}
	if err != nil {
		return nil, postgresError("blob.get", err)
	}
	return body, nil
}

func (f *Forge) pgBlobHead(ctx context.Context, key string) (*BlobInfo, error) {
	var info BlobInfo
	var size int64
	var modified time.Time
	var metadata []byte
	err := f.postgres(PrimitiveBlob).QueryRow(ctx, "SELECT size, content_type, etag, last_modified, metadata, cache_control, content_disposition, checksum_sha256 FROM forge_blobs WHERE key = $1", f.pgScoped(key)).Scan(&size, &info.ContentType, &info.ETag, &modified, &metadata, &info.CacheControl, &info.ContentDisposition, &info.ChecksumSha256)
	if err == pgx.ErrNoRows {
		return nil, nil
	}
	if err != nil {
		return nil, postgresError("blob.head", err)
	}
	if err := json.Unmarshal(metadata, &info.Metadata); err != nil {
		return nil, forgeError(CodeBackend, "blob.head", "stored metadata is malformed")
	}
	info.Key = key
	info.Size = uint64(size)
	info.LastModifiedMs = float64(modified.UnixMilli())
	return &info, nil
}

func nullableString(value string) any {
	if value == "" {
		return nil
	}
	return value
}

func (f *Forge) pgBlobDelete(ctx context.Context, key string) error {
	_, err := f.postgres(PrimitiveBlob).Exec(ctx, "DELETE FROM forge_blobs WHERE key = $1", f.pgScoped(key))
	if err != nil {
		return postgresError("blob.delete", err)
	}
	return nil
}

func (f *Forge) pgBlobList(ctx context.Context, prefix, after string, limit uint32) (BlobPage, error) {
	physicalPrefix := f.pgScoped(prefix)
	physicalAfter := ""
	if after != "" {
		physicalAfter = f.pgScoped(after)
	}
	rows, err := f.postgres(PrimitiveBlob).Query(ctx, "SELECT key, size, etag, last_modified FROM forge_blobs WHERE left(key, length($1)) = $1 AND key > $2 ORDER BY key LIMIT $3", physicalPrefix, physicalAfter, int64(limit)+1)
	if err != nil {
		return BlobPage{}, postgresError("blob.list", err)
	}
	defer rows.Close()
	items := make([]BlobSummary, 0, limit+1)
	for rows.Next() {
		var info BlobSummary
		var physical string
		var size int64
		var modified time.Time
		if err := rows.Scan(&physical, &size, &info.ETag, &modified); err != nil {
			return BlobPage{}, postgresError("blob.list", err)
		}
		info.Key = physical[len(f.namespace)+1:]
		info.Size = uint64(size)
		info.LastModifiedMs = float64(modified.UnixMilli())
		items = append(items, info)
	}
	if err := rows.Err(); err != nil {
		return BlobPage{}, postgresError("blob.list", err)
	}
	page := BlobPage{Items: items}
	if uint32(len(items)) > limit {
		page.Items = items[:limit]
		cursor := encodeCursor(page.Items[len(page.Items)-1].Key)
		page.Cursor = &cursor
	}
	return page, nil
}
