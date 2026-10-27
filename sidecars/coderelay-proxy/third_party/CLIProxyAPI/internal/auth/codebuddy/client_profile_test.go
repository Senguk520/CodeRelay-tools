package codebuddy

import (
	"net/http"
	"testing"
)

func TestNewClientProfileIsStablePerAuth(t *testing.T) {
	first := NewClientProfile("auth-a")
	second := NewClientProfile("auth-a")
	other := NewClientProfile("auth-b")

	if first.ProfileID == "" {
		t.Fatal("profile id must not be empty")
	}
	if first.ProfileID != second.ProfileID {
		t.Fatalf("same auth must keep one profile: %q != %q", first.ProfileID, second.ProfileID)
	}
	if first.ProfileID == other.ProfileID {
		t.Fatal("different auths must not share a profile id")
	}
	if first.ProductVersion != officialProductVersion {
		t.Fatalf("product version = %q, want %q", first.ProductVersion, officialProductVersion)
	}
}

func TestBuildHeadersAddsOnlyEvidenceBackedIdentity(t *testing.T) {
	creds := Creds{
		AccessToken: "token",
		UID:         "user-1",
		Profile:     NewClientProfile("auth-a"),
	}
	headers := BuildHeaders(creds, RequestHeaders{
		ModelID:   "deepseek-v4.1-flash",
		SessionID: "session-1",
	})

	if got := headers.Get("Authorization"); got != "Bearer token" {
		t.Fatalf("Authorization = %q", got)
	}
	if got := headers.Get("X-IDE-Version"); got != officialProductVersion {
		t.Fatalf("X-IDE-Version = %q", got)
	}
	if got := headers.Get("X-Product-Version"); got != officialProductVersion {
		t.Fatalf("X-Product-Version = %q", got)
	}
	if got := headers.Get("X-IDE-Type"); got != officialIDEType {
		t.Fatalf("X-IDE-Type = %q", got)
	}
	if got := headers.Get("User-Agent"); got != "CodeBuddyIDE/"+officialProductVersion {
		t.Fatalf("User-Agent = %q", got)
	}
	if got := headers.Get("X-Session-ID"); got != "session-1" {
		t.Fatalf("X-Session-ID = %q", got)
	}
	if got := headers.Get("X-Model-ID"); got != "deepseek-v4.1-flash" {
		t.Fatalf("X-Model-ID = %q", got)
	}
	if got := headers.Get("X-Request-ID"); len(got) != 32 {
		t.Fatalf("X-Request-ID = %q, want 32 hex chars", got)
	}
	if got := headers.Get("X-Trace-ID"); got != headers.Get("X-Request-ID") {
		t.Fatalf("X-Trace-ID = %q, want request id", got)
	}
	if got := headers.Get("X-Conversation-ID"); got != "" {
		t.Fatalf("unconfirmed conversation id must stay empty, got %q", got)
	}
	if got := headers.Get("X-Machine-ID"); got != "" {
		t.Fatalf("machine id must not be sent without evidence, got %q", got)
	}
}

func TestClientProfileClampsTrafficAndModeValues(t *testing.T) {
	profile := ClientProfile{
		RequestsPerMin: 99_999,
		MaxConcurrent:  999,
		MinIntervalMS:  999_999,
		JitterPercent:  99,
		HeaderMode:     "unknown",
	}.normalize("auth-a")
	if profile.RequestsPerMin != 6000 || profile.MaxConcurrent != 64 || profile.MinIntervalMS != 60_000 || profile.JitterPercent != 50 {
		t.Fatalf("profile not clamped: %#v", profile)
	}
	if profile.HeaderMode != HeaderModeEnforce {
		t.Fatalf("header mode = %q, want enforce", profile.HeaderMode)
	}
}

func TestBuildHeadersOmitsSessionWhenUnknown(t *testing.T) {
	headers := BuildHeaders(Creds{Profile: NewClientProfile("auth-a")}, RequestHeaders{})
	if got := headers.Get("X-Session-ID"); got != "" {
		t.Fatalf("unknown session must be omitted, got %q", got)
	}
}

func TestBuildHeadersOffModeKeepsLegacySurface(t *testing.T) {
	profile := NewClientProfile("auth-a")
	profile.HeaderMode = HeaderModeOff
	headers := BuildHeaders(Creds{Profile: profile}, RequestHeaders{ModelID: "m"})

	if got := headers.Get("User-Agent"); got != "CodeBuddyIDE" {
		t.Fatalf("off mode UA = %q, want legacy UA", got)
	}
	if got := headers.Get("X-IDE-Version"); got != "" {
		t.Fatalf("off mode must not add X-IDE-Version, got %q", got)
	}
	if got := headers.Get("X-Request-ID"); got != "" {
		t.Fatalf("off mode must not add request id, got %q", got)
	}
}

func TestProfileFromMetadataReadsClientProfile(t *testing.T) {
	profile := ProfileFromMetadata("auth-a", map[string]any{
		"client_profile": map[string]any{
			"profileId":      "p-1",
			"productVersion": "1.106.1",
			"headerMode":     HeaderModeOff,
		},
	})
	if profile.ProfileID != "p-1" || profile.HeaderMode != HeaderModeOff {
		t.Fatalf("profile = %#v", profile)
	}
}

func TestApplyHeadersForRequestCopiesBuiltHeaders(t *testing.T) {
	request, err := http.NewRequest(http.MethodPost, "https://example.invalid/v2/chat/completions", nil)
	if err != nil {
		t.Fatal(err)
	}
	ApplyHeadersForRequest(request, Creds{Profile: NewClientProfile("auth-a")}, RequestHeaders{ModelID: "m"})
	if got := request.Header.Get("X-Model-ID"); got != "m" {
		t.Fatalf("X-Model-ID = %q", got)
	}
	if got := request.Header.Get("X-Request-ID"); len(got) != 32 {
		t.Fatalf("X-Request-ID = %q", got)
	}
}
