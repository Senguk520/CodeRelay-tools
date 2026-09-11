package main

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	internalregistry "github.com/router-for-me/CLIProxyAPI/v7/internal/registry"
)

// codebuddyModelCacheFilename is the on-disk name of the persisted CodeBuddy
// model catalog cache. It lives next to the sidecar manifest so the model list
// survives restarts and account refreshes without an immediate backend round-trip.
const codebuddyModelCacheFilename = "codebuddy_models_cache.json"

// codebuddyModelCache is the persisted form of the CodeBuddy model catalog.
// Models are stored in full (not just IDs) so capability fields such as
// SupportsImages / ContextLength / MaxCompletionTokens survive a reload, which
// max_tokens clamping and image-capability checks depend on.
type codebuddyModelCache struct {
	Version  int                           `json:"version"`
	SyncedAt string                        `json:"syncedAt,omitempty"`
	Models   []*internalregistry.ModelInfo `json:"models"`
}

// codebuddyModelCachePath returns the cache file path for the given manifest
// path. It returns an empty string when the manifest path is blank.
func codebuddyModelCachePath(manifestPath string) string {
	if strings.TrimSpace(manifestPath) == "" {
		return ""
	}
	return filepath.Join(filepath.Dir(manifestPath), codebuddyModelCacheFilename)
}

// saveCodebuddyModelCache writes the model catalog to the cache file,
// overwriting any previous content.
func saveCodebuddyModelCache(path string, models []*internalregistry.ModelInfo) error {
	if strings.TrimSpace(path) == "" {
		return fmt.Errorf("codebuddy model cache path is empty")
	}
	cache := codebuddyModelCache{
		Version:  1,
		SyncedAt: time.Now().UTC().Format(time.RFC3339),
		Models:   models,
	}
	data, err := json.MarshalIndent(cache, "", "  ")
	if err != nil {
		return fmt.Errorf("marshal codebuddy model cache: %w", err)
	}
	if err := os.WriteFile(path, data, 0o644); err != nil {
		return fmt.Errorf("write codebuddy model cache: %w", err)
	}
	return nil
}

// loadCodebuddyModelCache reads the model catalog from the cache file. It
// returns an error when the file is absent, unreadable, malformed, or empty.
func loadCodebuddyModelCache(path string) ([]*internalregistry.ModelInfo, error) {
	if strings.TrimSpace(path) == "" {
		return nil, fmt.Errorf("codebuddy model cache path is empty")
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("read codebuddy model cache: %w", err)
	}
	var cache codebuddyModelCache
	if err := json.Unmarshal(data, &cache); err != nil {
		return nil, fmt.Errorf("parse codebuddy model cache: %w", err)
	}
	if len(cache.Models) == 0 {
		return nil, fmt.Errorf("codebuddy model cache is empty")
	}
	return cache.Models, nil
}
