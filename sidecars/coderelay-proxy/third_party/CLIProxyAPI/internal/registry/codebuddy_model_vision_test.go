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
// field for models without a measured override: models the catalog flags as
// image-capable report support (including deepseek-v4.x, which used to be
// hard-coded as text-only), while models the catalog does not flag report no
// support.
func TestCodebuddyModelSupportsImagesFollowsCatalog(t *testing.T) {
	installCodebuddyTestCatalog(t, []*ModelInfo{
		{ID: "deepseek-v4.1-flash", SupportsImages: true},
		{ID: "deepseek-v4-flash", SupportsImages: true},
		{ID: "deepseek-v4-pro", SupportsImages: true},
		{ID: "glm-5.3-flash", SupportsImages: true},
		{ID: "glm-5.3", SupportsImages: true},
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
		"glm-5.2",
		"hunyuan-2.0-thinking",
	}
	for _, id := range textOnly {
		if CodebuddyModelSupportsImages(id) {
			t.Errorf("catalog does not mark %q image-capable; CodebuddyModelSupportsImages should be false", id)
		}
	}
}

// TestCodebuddyModelSupportsImagesOverrides verifies that the measured
// capability override tables win over the catalog (2026-09-11 live measurement,
// see 《模型视觉能力实测与校正表.md》):
//   - glm-5v-turbo: catalog says image-capable, upstream refuses images → false.
//   - glm-5.1 / deepseek-v3-2-volc: catalog omits capability, upstream reads
//     images correctly → true.
func TestCodebuddyModelSupportsImagesOverrides(t *testing.T) {
	installCodebuddyTestCatalog(t, []*ModelInfo{
		// The catalog's own values must be overridden for these three.
		{ID: "glm-5v-turbo", SupportsImages: true},
		{ID: "glm-5.1", SupportsImages: false},
		{ID: "deepseek-v3-2-volc", SupportsImages: false},
		// A control model without an override keeps the catalog value.
		{ID: "glm-5.3", SupportsImages: true},
	})

	if CodebuddyModelSupportsImages("glm-5v-turbo") {
		t.Error("glm-5v-turbo must be excluded from native vision (measured: upstream refuses images)")
	}
	if !CodebuddyModelSupportsImages("glm-5.1") {
		t.Error("glm-5.1 must be included in native vision (measured: upstream reads images)")
	}
	if !CodebuddyModelSupportsImages("deepseek-v3-2-volc") {
		t.Error("deepseek-v3-2-volc must be included in native vision (measured: upstream reads images)")
	}
	// Case-insensitivity and whitespace trimming apply to the overrides too.
	if CodebuddyModelSupportsImages("  GLM-5V-TURBO ") {
		t.Error("glm-5v-turbo exclusion must be case-insensitive and trimmed")
	}
	if !CodebuddyModelSupportsImages("GLM-5.1") {
		t.Error("glm-5.1 inclusion must be case-insensitive")
	}
	// Control: no override → catalog value applies.
	if !CodebuddyModelSupportsImages("glm-5.3") {
		t.Error("un-overridden model must follow the catalog")
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
// catalog (or an empty ID) report no native vision support, so clients do not
// send images the upstream would reject.
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
