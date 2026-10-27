package executor

import (
	"encoding/json"
	"net/http"
	"strings"
	"testing"
)

func errorPayload(t *testing.T, sErr statusErr) map[string]any {
	t.Helper()
	var payload map[string]any
	if err := json.Unmarshal([]byte(sErr.msg), &payload); err != nil {
		t.Fatalf("expected normalized JSON error, got %q: %v", sErr.msg, err)
	}
	return payload
}

func TestNormalizeCodebuddyStatusErrContentReview(t *testing.T) {
	body := []byte(`{"code":11140,"msg":"request illegal","displayMsg":{"en":"The content did not pass the safety review.","zh":"内容未通过安全审核，请调整后重试。"}}`)
	sErr := normalizeCodebuddyStatusErr(http.StatusForbidden, body)
	payload := errorPayload(t, sErr)
	errObj, ok := payload["error"].(map[string]any)
	if !ok {
		t.Fatalf("missing error object: %v", payload)
	}
	if errObj["type"] != "content_filter" {
		t.Fatalf("type = %v, want content_filter", errObj["type"])
	}
	if errObj["code"] != "11140" {
		t.Fatalf("code = %v, want 11140", errObj["code"])
	}
	if errObj["message"] != "内容未通过安全审核，请调整后重试。" {
		t.Fatalf("message = %v, want zh displayMsg", errObj["message"])
	}
	if sErr.code != http.StatusForbidden {
		t.Fatalf("status = %d, want 403", sErr.code)
	}
}

func TestNormalizeCodebuddyStatusErrFallsBackToMsg(t *testing.T) {
	body := []byte(`{"code":10085,"msg":"请求不合法，请检查请求系统头"}`)
	sErr := normalizeCodebuddyStatusErr(http.StatusForbidden, body)
	payload := errorPayload(t, sErr)
	errObj := payload["error"].(map[string]any)
	if errObj["message"] != "请求不合法，请检查请求系统头" {
		t.Fatalf("message = %v, want msg fallback", errObj["message"])
	}
	if errObj["type"] != "invalid_request_error" {
		t.Fatalf("type = %v, want invalid_request_error", errObj["type"])
	}
}

func TestNormalizeCodebuddyStatusErrQuotaExhausted(t *testing.T) {
	body := []byte(`{"code":14018,"msg":"quota exhausted"}`)
	sErr := normalizeCodebuddyStatusErr(http.StatusForbidden, body)
	if sErr.code != http.StatusTooManyRequests {
		t.Fatalf("status = %d, want 429 for quota-exhausted business code", sErr.code)
	}
	payload := errorPayload(t, sErr)
	errObj := payload["error"].(map[string]any)
	if errObj["type"] != "rate_limit_error" {
		t.Fatalf("type = %v, want rate_limit_error", errObj["type"])
	}
}

func TestCodebuddyEffectiveStatusChannelBlockedIsNotAccountFailure(t *testing.T) {
	body := []byte(`{"code":11128,"msg":"Illegal API invocation from an unapproved channel"}`)
	if got := codebuddyEffectiveStatus(http.StatusForbidden, body); got != http.StatusBadRequest {
		t.Fatalf("11128 effective status = %d, want 400 so the account is not cooled", got)
	}
}

func TestNormalizeCodebuddyStatusErrPassthroughNonJSON(t *testing.T) {
	body := []byte("plain upstream error")
	sErr := normalizeCodebuddyStatusErr(http.StatusBadGateway, body)
	if sErr.msg != "plain upstream error" {
		t.Fatalf("msg = %q, want original body", sErr.msg)
	}
	if !strings.Contains(sErr.msg, "plain upstream error") {
		t.Fatalf("unexpected msg: %q", sErr.msg)
	}
}
