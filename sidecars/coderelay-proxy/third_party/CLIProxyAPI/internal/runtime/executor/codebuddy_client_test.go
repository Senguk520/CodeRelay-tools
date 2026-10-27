package executor

import (
	"context"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/router-for-me/CLIProxyAPI/v7/internal/auth/codebuddy"
	cliproxyauth "github.com/router-for-me/CLIProxyAPI/v7/sdk/cliproxy/auth"
	cliproxyexecutor "github.com/router-for-me/CLIProxyAPI/v7/sdk/cliproxy/executor"
	"github.com/tidwall/gjson"
)

func TestNormalizeCodebuddyDeveloperRole(t *testing.T) {
	body := []byte(`{"model":"deepseek-v4.1-flash","messages":[{"role":"developer","content":"x"},{"role":"user","content":"hi"}]}`)
	out, err := normalizeCodebuddyToolMessages(body)
	if err != nil {
		t.Fatal(err)
	}
	if got := gjson.GetBytes(out, "messages.0.role").String(); got != "system" {
		t.Fatalf("developer role = %q, want system", got)
	}
	if got := gjson.GetBytes(out, "messages.1.role").String(); got != "user" {
		t.Fatalf("user role changed: %q", got)
	}
}

func TestCodebuddyRequestHeadersUsesOnlyTrustedSessionIdentity(t *testing.T) {
	headers := codebuddyRequestHeaders("deepseek-v4.1-flash", cliproxyexecutor.Options{
		Headers: http.Header{
			"X-Session-Id":      {"inbound-session"},
			"X-Conversation-Id": {"inbound-conversation"},
		},
	})
	if headers.SessionID != "inbound-session" {
		t.Fatalf("session = %q", headers.SessionID)
	}
	if headers.ConversationID != "inbound-conversation" {
		t.Fatalf("conversation = %q", headers.ConversationID)
	}
	if headers.ModelID != "deepseek-v4.1-flash" {
		t.Fatalf("model = %q", headers.ModelID)
	}
	if len(headers.RequestID) != 32 || headers.TraceID != headers.RequestID {
		t.Fatalf("request/trace = %q/%q", headers.RequestID, headers.TraceID)
	}
}

func TestCodebuddyHeadersReachFakeUpstream(t *testing.T) {
	var received http.Header
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		received = r.Header.Clone()
		w.WriteHeader(http.StatusNoContent)
	}))
	defer server.Close()

	request, err := http.NewRequest(http.MethodPost, server.URL, nil)
	if err != nil {
		t.Fatal(err)
	}
	applyCodebuddyHeaders(request, codebuddy.Creds{
		AccessToken: "token",
		Profile:     codebuddy.NewClientProfile("fake-upstream"),
	}, codebuddy.RequestHeaders{ModelID: "deepseek-v4.1-flash", SessionID: "session-1"})
	response, err := http.DefaultClient.Do(request)
	if err != nil {
		t.Fatal(err)
	}
	defer response.Body.Close()

	if got := received.Get("X-IDE-Version"); got != "1.106.1" {
		t.Fatalf("X-IDE-Version = %q", got)
	}
	if got := received.Get("X-Session-ID"); got != "session-1" {
		t.Fatalf("X-Session-ID = %q", got)
	}
	if got := received.Get("X-Model-ID"); got != "deepseek-v4.1-flash" {
		t.Fatalf("X-Model-ID = %q", got)
	}
	if got := received.Get("Authorization"); got != "Bearer token" {
		t.Fatalf("Authorization = %q", got)
	}
}

func TestCodebuddyHTTPClientKeepsDefaultTransportUnlessExperiment(t *testing.T) {
	auth := &cliproxyauth.Auth{ID: "codebuddy-test"}
	off := codebuddyHTTPClient(context.Background(), nil, auth, codebuddy.ClientProfile{TLSMode: "off"})
	experiment := codebuddyHTTPClient(context.Background(), nil, auth, codebuddy.ClientProfile{TLSMode: "experiment"})

	if off.Transport != nil {
		t.Fatalf("default transport must stay on Go net/http, got %T", off.Transport)
	}
	if experiment.Transport == nil {
		t.Fatal("experiment mode must select the uTLS fallback transport")
	}
	if experiment.Transport == off.Transport {
		t.Fatal("experiment mode must not silently reuse the default transport")
	}
}
