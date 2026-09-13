package main

import (
	"encoding/json"
	"fmt"
	"net/http"
	"strings"
	"time"

	"github.com/gin-gonic/gin"
	codebuddyauth "github.com/router-for-me/CLIProxyAPI/v7/internal/auth/codebuddy"
	internalregistry "github.com/router-for-me/CLIProxyAPI/v7/internal/registry"
	"github.com/router-for-me/CLIProxyAPI/v7/internal/util"
	coreauth "github.com/router-for-me/CLIProxyAPI/v7/sdk/cliproxy/auth"
	sdktranslator "github.com/router-for-me/CLIProxyAPI/v7/sdk/translator"
	log "github.com/sirupsen/logrus"
)

// codebuddyImageToolModel is the placeholder CodeBuddy image model. The
// backend's dedicated image endpoint (/v2/images/generations) resolves the real
// image model from the client's requested model; this placeholder is registered
// so the model is visible under image generation mode and routable to the
// codebuddy provider.
const codebuddyImageToolModel = "codebuddy-image-1" // placeholder image model

func equalStringSlices(a, b []string) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}

// modelIDs returns a snapshot of the current model ID list. It is safe for
// concurrent use with setModelIDs.
func (m *manifest) modelIDs() []string {
	if m == nil {
		return nil
	}
	m.modelMu.RLock()
	defer m.modelMu.RUnlock()
	return m.ModelIDs
}

// setModelIDs atomically replaces the model ID list and reports whether the
// list actually changed.
func (m *manifest) setModelIDs(ids []string) bool {
	if m == nil {
		return false
	}
	m.modelMu.Lock()
	defer m.modelMu.Unlock()
	if equalStringSlices(m.ModelIDs, ids) {
		return false
	}
	m.ModelIDs = ids
	return true
}

// resolveRequestProviders determines the provider list backing a request model by
// consulting the global model registry. CodeBuddy models resolve to ["codebuddy"]
// while Codex models resolve to ["codex"]. Falls back to the legacy codex-only
// behavior when the model has no registered provider.
func resolveRequestProviders(model string) []string {
	seen := make(map[string]struct{})
	for _, candidate := range []string{util.ResolveAutoModel(model), model} {
		if candidate == "" {
			continue
		}
		if _, ok := seen[candidate]; ok {
			continue
		}
		seen[candidate] = struct{}{}
		if providers := util.GetProviderName(candidate); len(providers) > 0 {
			return providers
		}
	}
	return []string{"codex"}
}

func providersContain(providers []string, name string) bool {
	for _, p := range providers {
		if strings.EqualFold(strings.TrimSpace(p), name) {
			return true
		}
	}
	return false
}

// codebuddyModelsResponse mirrors the envelope returned by the official
// CodeBuddy backend model-list endpoint:
//
//	GET /v2/enterprises/personal/models
//	-> { "code":0, "msg":"OK", "data":{ "models":[ { "id":"glm-5.3", "tags":["craft"], ... } ] } }
type codebuddyModelsResponse struct {
	Code int    `json:"code"`
	Msg  string `json:"msg"`
	Data struct {
		Models []struct {
			ID               string   `json:"id"`
			Name             string   `json:"name"`
			Tags             []string `json:"tags"`
			SupportsImages   bool     `json:"supportsImages"`
			SupportsToolCall bool     `json:"supportsToolCall"`
			MaxOutputTokens  int      `json:"maxOutputTokens"`
			MaxInputTokens   int      `json:"maxInputTokens"`
		} `json:"models"`
	} `json:"data"`
}

// codebuddyModelSyncOutcome 记录一次 CodeBuddy 模型同步的最终结果，把失败阶段、
// HTTP 状态、业务码与所使用的账号一并带出。
//
// 背景（交接文档问题 3/8 的回归）：该同步原先在所有失败分支静默返回 nil，只随机
// 尝试第一个账号、失败不重试、不上报，导致一次偶发失败（账号 token 失效、网关
// 抖动、网络超时）后模型目录长期只剩 auto + codex-auto-review，界面还显示「已从
// 后端同步 N 个模型」，完全看不到原因。
type codebuddyModelSyncOutcome struct {
	Attempts      int    `json:"attempts"`
	AccountsTried int    `json:"accountsTried"`
	AccountID     string `json:"accountId,omitempty"`
	HTTPStatus    int    `json:"httpStatus,omitempty"`
	BizCode       int    `json:"bizCode,omitempty"`
	BizMsg        string `json:"bizMsg,omitempty"`
	Error         string `json:"error,omitempty"`
}

const (
	// codebuddyModelSyncMaxAccounts 限制单次同步最多尝试的账号数。账号池中存在
	// token 失效的账号时靠回退跳过，同时保证整体耗时可接受（同步请求会同步等待）。
	codebuddyModelSyncMaxAccounts = 8
	// codebuddyModelSyncMaxAttempts 是单个账号的最大尝试次数（仅网络类错误重试）。
	codebuddyModelSyncMaxAttempts = 3
	// codebuddyModelSyncTimeout 是单次请求超时。
	codebuddyModelSyncTimeout = 10 * time.Second
	// codebuddyModelSyncTotalBudget 是整次同步的总时间预算，避免账号池大面积异常时
	// 长时间占住调用方（界面「立即同步」与启动同步都要等它返回）。
	codebuddyModelSyncTotalBudget = 45 * time.Second
	// codebuddyModelSyncRetryBackoff 是退避重试的基础间隔（第 n 次重试等待 n 倍）。
	codebuddyModelSyncRetryBackoff = 400 * time.Millisecond
	// codebuddyModelSyncStartupRetryDelay 是启动同步失败后的补同步延迟（见 main.go）。
	codebuddyModelSyncStartupRetryDelay = 15 * time.Second
)

// codebuddyModelSyncCandidates 收集可用于拉取模型清单的中国站凭据。仅认 region=cn：
// 国际站（www.codebuddy.ai）账号体系暂未对外暴露，避免误取到国际站的模型目录。
func codebuddyModelSyncCandidates(auths []*coreauth.Auth) []codebuddyauth.Creds {
	candidates := make([]codebuddyauth.Creds, 0, len(auths))
	seen := make(map[string]struct{}, len(auths))
	for _, a := range auths {
		if a == nil {
			continue
		}
		c := codebuddyauth.CredsFromAuth(a)
		if !strings.EqualFold(strings.TrimSpace(c.Region), codebuddyauth.RegionCN) {
			continue
		}
		if strings.TrimSpace(c.AccessToken) == "" {
			continue
		}
		// 同一账号可能同时以文件凭据与 manifest 账号两种身份注册，按 uid（缺失时
		// 退回 token 本身）去重，避免重复请求后端。
		key := strings.TrimSpace(c.UID)
		if key == "" {
			key = c.AccessToken
		}
		if _, exists := seen[key]; exists {
			continue
		}
		seen[key] = struct{}{}
		candidates = append(candidates, c)
	}
	return candidates
}

// codebuddyModelFetchAttempt 是对单个账号的一次拉取结果。
type codebuddyModelFetchAttempt struct {
	models     []*internalregistry.ModelInfo
	status     int
	bizCode    int
	bizMsg     string
	err        error
	authFailed bool // 401/403：该账号凭据不可用，直接换账号
	retryable  bool // 网络类错误或 5xx/429：退避后重试
}

// fetchCodebuddyModelsOnce 用给定凭据拉取一次模型清单。失败时不返回错误中断整个同步，
// 而是把失败性质（凭据不可用 / 可重试 / 业务错误）交给调用方决定下一步。
func fetchCodebuddyModelsOnce(creds codebuddyauth.Creds) codebuddyModelFetchAttempt {
	result := codebuddyModelFetchAttempt{}
	req, err := http.NewRequest(http.MethodGet, creds.ResolveModelsURL(), nil)
	if err != nil {
		result.err = err
		return result
	}
	codebuddyauth.ApplyHeaders(req, creds)

	client := &http.Client{Timeout: codebuddyModelSyncTimeout}
	resp, err := client.Do(req)
	if err != nil {
		result.err = err
		result.retryable = true
		return result
	}
	defer func() { _ = resp.Body.Close() }()
	result.status = resp.StatusCode

	switch {
	case resp.StatusCode == http.StatusUnauthorized || resp.StatusCode == http.StatusForbidden:
		result.err = fmt.Errorf("账号凭据不可用（HTTP %d）", resp.StatusCode)
		result.authFailed = true
		return result
	case resp.StatusCode < 200 || resp.StatusCode >= 300:
		result.err = fmt.Errorf("后端返回 HTTP %d", resp.StatusCode)
		result.retryable = resp.StatusCode >= 500 || resp.StatusCode == http.StatusTooManyRequests
		return result
	}

	var envelope codebuddyModelsResponse
	if err := json.NewDecoder(resp.Body).Decode(&envelope); err != nil {
		result.err = fmt.Errorf("解析后端响应失败：%w", err)
		result.retryable = true
		return result
	}
	// 后端用 code != 0 表示业务错误。
	if envelope.Code != 0 {
		result.bizCode = envelope.Code
		result.bizMsg = strings.TrimSpace(envelope.Msg)
		if result.bizMsg != "" {
			result.err = fmt.Errorf("后端业务错误 code=%d：%s", envelope.Code, result.bizMsg)
		} else {
			result.err = fmt.Errorf("后端业务错误 code=%d", envelope.Code)
		}
		return result
	}

	// 过滤非对话模型（如 text-to-image），与官方客户端 listAvailableModels 行为一致。
	nonChatTags := map[string]bool{"text-to-image": true}
	models := make([]*internalregistry.ModelInfo, 0, len(envelope.Data.Models))
	for _, m := range envelope.Data.Models {
		if strings.TrimSpace(m.ID) == "" {
			continue
		}
		hasNonChat := false
		for _, t := range m.Tags {
			if nonChatTags[strings.ToLower(strings.TrimSpace(t))] {
				hasNonChat = true
				break
			}
		}
		if hasNonChat {
			continue
		}
		models = append(models, &internalregistry.ModelInfo{
			ID:                  m.ID,
			Name:                m.Name,
			Object:              "model",
			OwnedBy:             "tencent",
			Type:                "codebuddy",
			SupportsImages:      m.SupportsImages,
			ContextLength:       m.MaxInputTokens,
			MaxCompletionTokens: m.MaxOutputTokens,
		})
	}
	result.models = models
	return result
}

// syncCodebuddyModelsFromBackend fetches the authoritative CodeBuddy model list
// from the official Tencent backend. 账号池中任意 region=cn 且带 access_token 的
// 账号都可作凭据：按顺序尝试，401/403 自动换下一个账号，网络类错误退避重试；成功后
// 安装到 registry、写入本地缓存并返回去重排序后的模型 ID 列表。
//
// outcome 始终带出失败原因与尝试明细（账号数/次数/HTTP 状态/业务码），调用方据此
// 写日志、发事件并回传界面，不再出现「同步失败但无人知道」的静默降级。
//
// This is the preferred source over app.asar extraction because the backend
// exposes models (e.g. glm-5.3) that may not yet be bundled in the local client.
func syncCodebuddyModelsFromBackend(auths []*coreauth.Auth, cachePath string) ([]string, codebuddyModelSyncOutcome) {
	outcome := codebuddyModelSyncOutcome{}
	candidates := codebuddyModelSyncCandidates(auths)
	if len(candidates) == 0 {
		outcome.Error = "没有可用的中国站账号（需要 region=cn 且带 access_token 的账号）"
		return nil, outcome
	}

	maxAccounts := codebuddyModelSyncMaxAccounts
	if len(candidates) < maxAccounts {
		maxAccounts = len(candidates)
	}
	deadline := time.Now().Add(codebuddyModelSyncTotalBudget)

	lastError := ""
	for index := 0; index < maxAccounts; index++ {
		creds := candidates[index]
		outcome.AccountsTried = index + 1
		outcome.AccountID = strings.TrimSpace(creds.UID)

		for attempt := 1; attempt <= codebuddyModelSyncMaxAttempts; attempt++ {
			outcome.Attempts++
			result := fetchCodebuddyModelsOnce(creds)
			outcome.HTTPStatus = result.status
			outcome.BizCode = result.bizCode
			outcome.BizMsg = result.bizMsg

			if len(result.models) > 0 {
				// 直接信任后端模型清单，不做逐模型推理探测：探测请求（无论参数形态）
				// 在部分路由上会被整体拒绝并返回 11102，导致大量可用模型被误过滤。
				// 模型是否真的可用以实际推理请求的结果为准，由请求日志与账号健康度呈现。
				ids := internalregistry.InstallCodebuddyModels(result.models)
				if len(ids) == 0 {
					outcome.Error = "后端返回的模型清单为空或全部被过滤"
					return nil, outcome
				}
				// 同步成功后持久化到本地缓存，供下次启动/刷新账号时兜底加载，避免
				// 依赖异步后端同步导致的「未查询到可用模型」真空期。
				if strings.TrimSpace(cachePath) != "" {
					if err := saveCodebuddyModelCache(cachePath, result.models); err != nil {
						log.Warnf("codebuddy model sync: persist cache failed: %v", err)
					}
				}
				outcome.Error = ""
				return ids, outcome
			}

			if result.err != nil {
				lastError = result.err.Error()
			} else {
				lastError = "后端未返回模型清单"
			}
			if result.authFailed {
				break // 凭据不可用：换下一个账号
			}
			if !result.retryable || attempt == codebuddyModelSyncMaxAttempts || time.Now().After(deadline) {
				break
			}
			time.Sleep(codebuddyModelSyncRetryBackoff * time.Duration(attempt))
		}
		if time.Now().After(deadline) {
			break
		}
	}

	outcome.Error = lastError
	if outcome.Error == "" {
		outcome.Error = "未知错误"
	}
	return nil, outcome
}

// handleCodebuddySyncModels 立即触发一次 CodeBuddy 模型同步，刷新 manifest 与
// /v1/models 响应。模型清单仅以腾讯后端为准（含 app.asar 未打包的新模型，
// 如 glm-5.3），不做 app.asar / 本地注册表回退。
func (s *relayServer) handleCodebuddySyncModels(c *gin.Context) {
	if _, ok := s.requireAPIKey(c); !ok {
		return
	}

	var synced []string
	outcome := codebuddyModelSyncOutcome{}
	source := "tencent-backend"

	// 仅从腾讯后端动态拉取（需要 CodeBuddy 账号 access_token）。
	if s.authManager != nil {
		synced, outcome = syncCodebuddyModelsFromBackend(s.authManager.List(), codebuddyModelCachePath(s.manifestPath))
	} else {
		outcome.Error = "内部错误：账号管理器不可用"
	}

	refreshed := false
	if s.manifest != nil && len(synced) > 0 {
		refreshed = s.manifest.setModelIDs(synced)
	}
	if refreshed {
		internalregistry.NotifyCodebuddyModelRefresh()
	}
	// 失败时把原因写进日志并发事件：界面与服务页均可见，避免再次出现「同步失败但
	// 界面显示已同步 2 个模型」的误导。响应仍是 200，前端按 error/count 判断结果。
	if len(synced) == 0 {
		message := "CodeBuddy 模型同步失败：" + outcome.Error
		log.Warnf("codebuddy model sync failed: %s (accounts=%d attempts=%d httpStatus=%d bizCode=%d)",
			outcome.Error, outcome.AccountsTried, outcome.Attempts, outcome.HTTPStatus, outcome.BizCode)
		if s.emitter != nil {
			s.emitter.emit(map[string]any{
				"type":    "codebuddy_model_sync_error",
				"message": message,
			})
		}
	}
	c.JSON(http.StatusOK, gin.H{
		"version":       1,
		"count":         len(synced),
		"refreshed":     refreshed,
		"source":        source,
		"models":        synced,
		"attempts":      outcome.Attempts,
		"accountsTried": outcome.AccountsTried,
		"accountId":     outcome.AccountID,
		"httpStatus":    outcome.HTTPStatus,
		"bizCode":       outcome.BizCode,
		"error":         outcome.Error,
	})
}

// handleCodebuddyImagesRelay relays an OpenAI Images API request to the
// CodeBuddy backend's dedicated image endpoint (/v2/images/generations or
// /v2/images/edits). That endpoint returns a single JSON document (not an SSE
// stream), so this path is non-streaming.
func (s *relayServer) handleCodebuddyImagesRelay(c *gin.Context, imageReq imageRelayRequest, requestedModel string) {
	body := imageReq.rawBody
	if len(body) == 0 {
		// Multipart edits have no raw JSON body; fall back to the Responses-style
		// body is not applicable to CodeBuddy, so surface a clear error instead.
		writeAPIError(c, http.StatusBadRequest, "multipart image edits are not supported for CodeBuddy; use the JSON images API", "unsupported_media_type")
		return
	}
	req, opts := buildExecutorRequest(c, body, requestedModel, sdktranslator.FromString("openai-image"), "", false)
	startedAt := time.Now()
	s.emitExecutorDiagnostic(c, "image_execute", requestedModel, "execute", startedAt, "codebuddy images endpoint")
	resp, err := s.runtime.Execute(relayContext(c), []string{"codebuddy"}, req, opts)
	if err != nil {
		s.writeExecutorError(c, err)
		return
	}
	writeUpstreamHeaders(c.Writer.Header(), resp.Headers)
	c.Data(http.StatusOK, "application/json", resp.Payload)
}
