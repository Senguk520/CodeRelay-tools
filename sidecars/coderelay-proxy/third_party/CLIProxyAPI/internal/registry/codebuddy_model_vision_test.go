package registry

import "testing"

// installCodebuddyTestCatalog pins the package-level synced catalog to a fixed
// model list for the duration of a test. The catalog is a process-wide global,
// so every capability test installs its own catalog and restores the previous
// one on cleanup to avoid cross-test pollution.
func installCodebuddyTestCatalog(t *testing.T, models []*ModelInfo) {
	t.Helper()
	codebuddySyncMu.Lock()
	prev := codebuddySynced
	codebuddySynced = models
	codebuddySyncMu.Unlock()
	t.Cleanup(func() {
		codebuddySyncMu.Lock()
		codebuddySynced = prev
		codebuddySyncMu.Unlock()
	})
}

// TestCodebuddyModelSupportsImagesFollowsCatalog verifies that native vision
// capability is taken verbatim from the online model catalog's supportsImages
// field: models the catalog flags as image-capable report support (including
// deepseek-v4.x, which used to be hard-coded as text-only), while models the
// catalog does not flag report no support.
func TestCodebuddyModelSupportsImagesFollowsCatalog(t *testing.T) {
	installCodebuddyTestCatalog(t, []*ModelInfo{
		{ID: "deepseek-v4.1-flash", SupportsImages: true},
		{ID: "deepseek-v4-flash", SupportsImages: true},
		{ID: "deepseek-v4-pro", SupportsImages: true},
		{ID: "glm-5.3-flash", SupportsImages: true},
		{ID: "glm-5.3", SupportsImages: true},
		{ID: "glm-5.1", SupportsImages: false},
		{ID: "glm-5.2", SupportsImages: false},
		{ID: "hunyuan-2.0-thinking", SupportsImages: false},
	})

	visionCapable := []string{
		"deepseek-v4.1-flash",
		"deepseek-v4-flash",
		"deepseek-v4-pro",
		"glm-5.3-flash",
		"glm-5.3",
	}
	for _, id := range visionCapable {
		if !CodebuddyModelSupportsImages(id) {
			t.Errorf("catalog marks %q image-capable; CodebuddyModelSupportsImages should be true", id)
		}
	}

	textOnly := []string{
		"glm-5.1",
		"glm-5.2",
		"hunyuan-2.0-thinking",
	}
	for _, id := range textOnly {
		if CodebuddyModelSupportsImages(id) {
			t.Errorf("catalog does not mark %q image-capable; CodebuddyModelSupportsImages should be false", id)
		}
	}
}

// TestCodebuddyModelSupportsImagesCaseInsensitive verifies the catalog lookup is
// case-insensitive.
func TestCodebuddyModelSupportsImagesCaseInsensitive(t *testing.T) {
	installCodebuddyTestCatalog(t, []*ModelInfo{
		{ID: "GLM-5.3-Flash", SupportsImages: true},
	})
	if !CodebuddyModelSupportsImages("glm-5.3-flash") {
		t.Error("catalog lookup should be case-insensitive")
	}
}

// TestCodebuddyModelSupportsImagesUnknown verifies that models absent from the
// catalog (or an empty ID) report no native vision support, so the vision-proxy
// layer still routes their image inputs through the vision model.
func TestCodebuddyModelSupportsImagesUnknown(t *testing.T) {
	installCodebuddyTestCatalog(t, []*ModelInfo{
		{ID: "glm-5.3", SupportsImages: true},
	})
	unknown := []string{
		"hunyuan-2.0-instruct",
		"unknown-model",
		"",
	}
	for _, id := range unknown {
		if CodebuddyModelSupportsImages(id) {
			t.Errorf("model %q is not in the catalog; should NOT report vision support", id)
		}
	}
}

// TestCodebuddyModelMaxCompletionTokens verifies the max-completion ceiling
// lookup falls back to the shared default for unknown/empty model IDs.
func TestCodebuddyModelMaxCompletionTokens(t *testing.T) {
	if got := CodebuddyModelMaxCompletionTokens(""); got != CodebuddyMaxCompletionTokensDefault {
		t.Fatalf("empty model = %d, want %d", got, CodebuddyMaxCompletionTokensDefault)
	}
	if got := CodebuddyModelMaxCompletionTokens("unknown-model"); got != CodebuddyMaxCompletionTokensDefault {
		t.Fatalf("unknown model = %d, want %d", got, CodebuddyMaxCompletionTokensDefault)
	}
	// A known CodeBuddy model should resolve to a positive ceiling (32768).
	if got := CodebuddyModelMaxCompletionTokens("deepseek-v4-pro"); got <= 0 {
		t.Fatalf("known model = %d, want > 0", got)
	}
}
