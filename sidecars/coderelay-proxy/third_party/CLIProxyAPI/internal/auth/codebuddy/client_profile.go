package codebuddy

import (
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"net/http"
	"strings"

	log "github.com/sirupsen/logrus"
)

const (
	// These values come from the official CodeBuddy CN installation under
	// resources/app/package.json and product-ide-cn.json (see
	// upstream/codebuddy-client-profile.md). Do not add speculative values here.
	officialProductVersion = "1.106.1"
	officialPluginVersion  = "4.12.2"
	officialIDEType        = "CodeBuddyIDE"
	officialProductName    = "CodeBuddy"

	HeaderModeOff     = "off"
	HeaderModeShadow  = "shadow"
	HeaderModeEnforce = "enforce"
)

// ClientProfile is the per-account, stable client identity used for CodeBuddy
// requests. It is persisted with the account so token refresh never changes it.
type ClientProfile struct {
	ProfileID       string `json:"profileId,omitempty"`
	ProfileVersion  int    `json:"profileVersion,omitempty"`
	Platform        string `json:"platform,omitempty"`
	PlatformVersion string `json:"platformVersion,omitempty"`
	ProductName     string `json:"productName,omitempty"`
	ProductVersion  string `json:"productVersion,omitempty"`
	IDEType         string `json:"ideType,omitempty"`
	IDEName         string `json:"ideName,omitempty"`
	PluginName      string `json:"pluginName,omitempty"`
	PluginVersion   string `json:"pluginVersion,omitempty"`
	BaseUserAgent   string `json:"baseUserAgent,omitempty"`
	HeaderMode      string `json:"headerMode,omitempty"`
	TLSMode         string `json:"tlsMode,omitempty"` // off | experiment
	HeaderPolicy    int    `json:"headerPolicyVersion,omitempty"`
	RequestsPerMin  int    `json:"requestsPerMinute,omitempty"`
	MaxConcurrent   int    `json:"maxConcurrent,omitempty"`
	MinIntervalMS   int    `json:"minIntervalMs,omitempty"`
	JitterPercent   int    `json:"jitterPercent,omitempty"`
}

// RequestHeaders carries per-request identities. Session and conversation IDs
// must come from a trusted inbound source; they are never invented per request.
type RequestHeaders struct {
	RequestID      string
	TraceID        string
	SessionID      string
	ConversationID string
	ModelID        string
}

// NewRequestHeaders returns an empty request identity set.
func NewRequestHeaders() RequestHeaders {
	return RequestHeaders{}
}

// EnsureIDs fills request/trace IDs exactly once.
func (r RequestHeaders) EnsureIDs() RequestHeaders {
	if strings.TrimSpace(r.RequestID) == "" {
		r.RequestID = newRequestID()
	}
	if strings.TrimSpace(r.TraceID) == "" {
		r.TraceID = r.RequestID
	}
	return r
}

// NewClientProfile creates the default profile for an auth record. The ID is a
// stable digest of the auth ID, so legacy auth files that predate profiles still
// get the same identity after a restart.
func NewClientProfile(authID string) ClientProfile {
	sum := sha256.Sum256([]byte("codebuddy-client-profile:" + strings.TrimSpace(authID)))
	return ClientProfile{
		ProfileID:      hex.EncodeToString(sum[:16]),
		ProfileVersion: 1,
		Platform:       officialIDEType,
		ProductName:    officialProductName,
		ProductVersion: officialProductVersion,
		IDEType:        officialIDEType,
		IDEName:        officialIDEType,
		PluginName:     "coding-copilot",
		PluginVersion:  officialPluginVersion,
		BaseUserAgent:  "CodeBuddyIDE/" + officialProductVersion,
		HeaderMode:     HeaderModeEnforce,
		TLSMode:        "off",
		HeaderPolicy:   2,
		RequestsPerMin: 240,
		MaxConcurrent:  4,
		MinIntervalMS:  250,
		JitterPercent:  10,
	}.normalize(authID)
}

// ProfileFromMetadata accepts the camelCase profile object persisted by the host.
func ProfileFromMetadata(authID string, metadata map[string]any) ClientProfile {
	if metadata == nil {
		return NewClientProfile(authID)
	}
	raw, ok := metadata["client_profile"]
	if !ok {
		raw = metadata["clientProfile"]
	}
	if raw == nil {
		return NewClientProfile(authID)
	}
	data, err := json.Marshal(raw)
	if err != nil {
		return NewClientProfile(authID)
	}
	var profile ClientProfile
	if err := json.Unmarshal(data, &profile); err != nil {
		return NewClientProfile(authID)
	}
	return profile.normalize(authID)
}

func (p ClientProfile) normalize(authID string) ClientProfile {
	if strings.TrimSpace(p.ProfileID) == "" {
		sum := sha256.Sum256([]byte("codebuddy-client-profile:" + strings.TrimSpace(authID)))
		p.ProfileID = hex.EncodeToString(sum[:16])
	}
	if p.ProfileVersion <= 0 {
		p.ProfileVersion = 1
	}
	if strings.TrimSpace(p.Platform) == "" {
		p.Platform = officialIDEType
	}
	if strings.TrimSpace(p.ProductName) == "" {
		p.ProductName = officialProductName
	}
	if strings.TrimSpace(p.ProductVersion) == "" {
		p.ProductVersion = officialProductVersion
	}
	if strings.TrimSpace(p.IDEType) == "" {
		p.IDEType = officialIDEType
	}
	if strings.TrimSpace(p.IDEName) == "" {
		p.IDEName = officialIDEType
	}
	if strings.TrimSpace(p.PluginName) == "" {
		p.PluginName = "coding-copilot"
	}
	if strings.TrimSpace(p.PluginVersion) == "" {
		p.PluginVersion = officialPluginVersion
	}
	if strings.TrimSpace(p.BaseUserAgent) == "" {
		p.BaseUserAgent = "CodeBuddyIDE/" + p.ProductVersion
	}
	switch strings.ToLower(strings.TrimSpace(p.HeaderMode)) {
	case HeaderModeOff:
		p.HeaderMode = HeaderModeOff
	case HeaderModeShadow:
		p.HeaderMode = HeaderModeShadow
	default:
		p.HeaderMode = HeaderModeEnforce
	}
	if strings.EqualFold(strings.TrimSpace(p.TLSMode), "experiment") {
		p.TLSMode = "experiment"
	} else {
		p.TLSMode = "off"
	}
	if p.HeaderPolicy <= 0 {
		p.HeaderPolicy = 2
	}
	if p.RequestsPerMin <= 0 {
		p.RequestsPerMin = 240
	}
	if p.RequestsPerMin > 6000 {
		p.RequestsPerMin = 6000
	}
	if p.MaxConcurrent <= 0 {
		p.MaxConcurrent = 4
	}
	if p.MaxConcurrent > 64 {
		p.MaxConcurrent = 64
	}
	if p.MinIntervalMS < 0 {
		p.MinIntervalMS = 0
	}
	if p.MinIntervalMS > 60_000 {
		p.MinIntervalMS = 60_000
	}
	if p.JitterPercent < 0 {
		p.JitterPercent = 0
	}
	if p.JitterPercent > 50 {
		p.JitterPercent = 50
	}
	return p
}

// BuildHeaders returns the complete upstream header set. Only fields with
// evidence in the official client are emitted; unknown fingerprint fields stay
// absent instead of being fabricated.
func BuildHeaders(creds Creds, request RequestHeaders) http.Header {
	profile := creds.Profile.normalize(creds.AuthID)
	legacy := legacyHeaders(creds)
	if profile.HeaderMode == HeaderModeOff {
		return legacy
	}
	request = request.EnsureIDs()
	official := legacy.Clone()
	official.Set("X-IDE-Version", profile.ProductVersion)
	official.Set("X-Product-Version", profile.ProductVersion)
	official.Set("X-IDE-Type", profile.IDEType)
	official.Set("User-Agent", profile.BaseUserAgent)
	official.Set("X-Request-ID", request.RequestID)
	official.Set("X-Trace-ID", request.TraceID)
	if request.SessionID != "" {
		official.Set("X-Session-ID", request.SessionID)
	}
	if request.ConversationID != "" {
		official.Set("X-Conversation-ID", request.ConversationID)
	}
	if request.ModelID != "" {
		official.Set("X-Model-ID", request.ModelID)
	}
	if profile.HeaderMode == HeaderModeShadow {
		log.Debugf("codebuddy profile shadow account=%s profile=%s product=%s", creds.AuthID, profile.ProfileID, profile.ProductVersion)
		return legacy
	}
	return official
}

// ApplyHeadersForRequest applies the built headers to an outgoing request.
func ApplyHeadersForRequest(r *http.Request, creds Creds, request RequestHeaders) {
	if r == nil {
		return
	}
	for name, values := range BuildHeaders(creds, request) {
		r.Header.Del(name)
		for _, value := range values {
			r.Header.Add(name, value)
		}
	}
}

// ApplyHeaders is retained for callers without request identities.
func ApplyHeaders(r *http.Request, c Creds) {
	ApplyHeadersForRequest(r, c, NewRequestHeaders())
}

func legacyHeaders(creds Creds) http.Header {
	headers := make(http.Header)
	headers.Set("Content-Type", "application/json")
	headers.Set("Accept", "application/json")
	if creds.AccessToken != "" {
		headers.Set("Authorization", "Bearer "+creds.AccessToken)
	}
	if creds.UID != "" {
		headers.Set("X-User-Id", creds.UID)
	}
	if creds.EnterpriseID != "" {
		headers.Set("X-Enterprise-Id", creds.EnterpriseID)
		headers.Set("X-Tenant-Id", creds.EnterpriseID)
	}
	if creds.Domain != "" {
		headers.Set("X-Domain", creds.Domain)
	}
	headers.Set("X-Product", "SaaS")
	headers.Set("X-IDE-Name", officialIDEType)
	headers.Set("X-Requested-With", "XMLHttpRequest")
	headers.Set("User-Agent", officialIDEType)
	return headers
}

func newRequestID() string {
	var buf [16]byte
	if _, err := rand.Read(buf[:]); err != nil {
		// crypto/rand failure is exceptional; a timestamp-like fallback would be
		// worse than omitting the request ID, so callers get an empty value.
		return ""
	}
	return hex.EncodeToString(buf[:])
}
