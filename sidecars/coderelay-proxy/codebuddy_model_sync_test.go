package main

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"

	internalregistry "github.com/router-for-me/CLIProxyAPI/v7/internal/registry"
	coreauth "github.com/router-for-me/CLIProxyAPI/v7/sdk/cliproxy/auth"
)

// codebuddyTestAuth 构造一个测试用 auth 记录：凭据以顶层 JSON 字段写入 Metadata，
// base_url 指向 httptest 服务器，从而让模型同步打到本地测试端点而不是真实后端。
func codebuddyTestAuth(id, baseURL, accessToken, region string) *coreauth.Auth {
	metadata := map[string]any{
		"type":     "codebuddy",
		"uid":      id,
		"domain":   "www.workbuddy.cn",
		"base_url": baseURL,
	}
	if region != "" {
		metadata["region"] = region
	}
	if accessToken != "" {
		metadata["access_token"] = accessToken
	}
	return &coreauth.Auth{ID: id, Provider: "codebuddy", Metadata: metadata}
}

// codebuddyTestModelsPayload 生成与真实后端同构的模型清单响应。
func codebuddyTestModelsPayload(ids ...string) []byte {
	type model struct {
		ID              string   `json:"id"`
		Name            string   `json:"name"`
		Tags            []string `json:"tags"`
		SupportsImages  bool     `json:"supportsImages"`
		MaxInputTokens  int      `json:"maxInputTokens"`
		MaxOutputTokens int      `json:"maxOutputTokens"`
	}
	models := make([]model, 0, len(ids))
	for _, id := range ids {
		models = append(models, model{
			ID:              id,
			Name:            id,
			Tags:            []string{"craft"},
			MaxInputTokens:  200000,
			MaxOutputTokens: 32000,
		})
	}
	body := map[string]any{
		"code": 0,
		"msg":  "OK",
		"data": map[string]any{"models": models},
	}
	data, err := json.Marshal(body)
	if err != nil {
		panic(err)
	}
	return data
}

func codebuddyTestCachePath(t *testing.T) string {
	t.Helper()
	return codebuddyModelCachePath(filepath.Join(t.TempDir(), "manifest.json"))
}

// isolateCodebuddyCatalog 保证用例开始与结束时注册表里的在线模型目录都为空：同包
// 其它用例（如 manifest_policy_test.go 的能力判定）依赖「目录为空时回退静态
// models.json」的前提，而测试顺序不保证，不能把测试安装的目录留给它们。
func isolateCodebuddyCatalog(t *testing.T) {
	t.Helper()
	internalregistry.ResetCodebuddyModelCatalogForTest()
	t.Cleanup(internalregistry.ResetCodebuddyModelCatalogForTest)
}

// TestCodebuddyModelSyncFallsBackToNextAccount：首个账号凭据失效（401）时必须自动
// 换下一个账号，整体仍应成功，且把实际使用的账号带出。
func TestCodebuddyModelSyncFallsBackToNextAccount(t *testing.T) {
	isolateCodebuddyCatalog(t)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if strings.Contains(r.Header.Get("Authorization"), "bad-token") {
			http.Error(w, `{"code":11140,"msg":"request illegal"}`, http.StatusUnauthorized)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write(codebuddyTestModelsPayload("auto", "glm-5.3"))
	}))
	defer server.Close()

	cachePath := codebuddyTestCachePath(t)
	auths := []*coreauth.Auth{
		codebuddyTestAuth("acct-bad", server.URL, "bad-token", "cn"),
		codebuddyTestAuth("acct-good", server.URL, "good-token", "cn"),
	}
	ids, outcome := syncCodebuddyModelsFromBackend(auths, cachePath)
	if len(ids) != 2 {
		t.Fatalf("ids = %v, want 2 entries (outcome=%+v)", ids, outcome)
	}
	if outcome.AccountID != "acct-good" {
		t.Fatalf("outcome.AccountID = %q, want acct-good", outcome.AccountID)
	}
	if outcome.AccountsTried != 2 {
		t.Fatalf("outcome.AccountsTried = %d, want 2", outcome.AccountsTried)
	}
	if outcome.Error != "" {
		t.Fatalf("outcome.Error = %q, want empty", outcome.Error)
	}
	if _, err := loadCodebuddyModelCache(cachePath); err != nil {
		t.Fatalf("cache should be persisted on success: %v", err)
	}
}

// TestCodebuddyModelSyncReportsAllAccountsFailed：全部账号都失败时必须返回具体原因
// （含 HTTP 状态）且不写缓存、不动已有清单。
func TestCodebuddyModelSyncReportsAllAccountsFailed(t *testing.T) {
	isolateCodebuddyCatalog(t)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		http.Error(w, `{"code":11140,"msg":"request illegal"}`, http.StatusUnauthorized)
	}))
	defer server.Close()

	cachePath := codebuddyTestCachePath(t)
	auths := []*coreauth.Auth{
		codebuddyTestAuth("acct-1", server.URL, "t1", "cn"),
		codebuddyTestAuth("acct-2", server.URL, "t2", "cn"),
	}
	ids, outcome := syncCodebuddyModelsFromBackend(auths, cachePath)
	if len(ids) != 0 {
		t.Fatalf("ids = %v, want empty", ids)
	}
	if !strings.Contains(outcome.Error, "401") {
		t.Fatalf("outcome.Error = %q, want it to mention HTTP 401", outcome.Error)
	}
	if outcome.AccountsTried != 2 || outcome.Attempts != 2 {
		t.Fatalf("accounts/attempts = %d/%d, want 2/2", outcome.AccountsTried, outcome.Attempts)
	}
	if _, err := loadCodebuddyModelCache(cachePath); err == nil {
		t.Fatal("cache must not be written when every account fails")
	}
}

// TestCodebuddyModelSyncRetriesTransientFailure：5xx 属于可重试失败，第三次尝试成功。
func TestCodebuddyModelSyncRetriesTransientFailure(t *testing.T) {
	isolateCodebuddyCatalog(t)
	var calls int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if atomic.AddInt32(&calls, 1) < 3 {
			http.Error(w, "upstream boom", http.StatusBadGateway)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write(codebuddyTestModelsPayload("auto", "hy3"))
	}))
	defer server.Close()

	ids, outcome := syncCodebuddyModelsFromBackend(
		[]*coreauth.Auth{codebuddyTestAuth("acct-1", server.URL, "t", "cn")},
		codebuddyTestCachePath(t),
	)
	if len(ids) != 2 {
		t.Fatalf("ids = %v, want 2 entries (outcome=%+v)", ids, outcome)
	}
	if outcome.Attempts != 3 {
		t.Fatalf("outcome.Attempts = %d, want 3", outcome.Attempts)
	}
}

// TestCodebuddyModelSyncReportsBusinessError：后端 200 但业务码非 0 时，原因里要带上业务码。
func TestCodebuddyModelSyncReportsBusinessError(t *testing.T) {
	isolateCodebuddyCatalog(t)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"code":11102,"msg":"当前模型不可用"}`))
	}))
	defer server.Close()

	ids, outcome := syncCodebuddyModelsFromBackend(
		[]*coreauth.Auth{codebuddyTestAuth("acct-1", server.URL, "t", "cn")},
		codebuddyTestCachePath(t),
	)
	if len(ids) != 0 {
		t.Fatalf("ids = %v, want empty", ids)
	}
	if !strings.Contains(outcome.Error, "11102") {
		t.Fatalf("outcome.Error = %q, want it to mention business code 11102", outcome.Error)
	}
	if outcome.BizCode != 11102 {
		t.Fatalf("outcome.BizCode = %d, want 11102", outcome.BizCode)
	}
}

// TestCodebuddyModelSyncRequiresChineseRegionAccount：国际站账号或缺少 token 时不算
// 候选，必须给出可读原因而不是静默失败。
func TestCodebuddyModelSyncRequiresChineseRegionAccount(t *testing.T) {
	isolateCodebuddyCatalog(t)
	auths := []*coreauth.Auth{
		codebuddyTestAuth("intl", "https://example.invalid", "t", "intl"),
		codebuddyTestAuth("cn-without-token", "https://example.invalid", "", "cn"),
		codebuddyTestAuth("cn-without-region", "https://example.invalid", "t", ""),
	}
	ids, outcome := syncCodebuddyModelsFromBackend(auths, codebuddyTestCachePath(t))
	if len(ids) != 0 {
		t.Fatalf("ids = %v, want empty", ids)
	}
	if !strings.Contains(outcome.Error, "中国站账号") {
		t.Fatalf("outcome.Error = %q, want it to explain that no CN account is available", outcome.Error)
	}
}
