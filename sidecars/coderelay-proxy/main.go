package main

import (
	"context"

	"errors"
	"flag"
	"fmt"

	"net"
	"net/http"

	"os"
	"os/signal"
	"path/filepath"

	"strconv"
	"strings"

	"syscall"
	"time"

	codexlive "github.com/router-for-me/CLIProxyAPI/v7/internal/client/codex/live"
	internalregistry "github.com/router-for-me/CLIProxyAPI/v7/internal/registry"
	log "github.com/sirupsen/logrus"

	sdkhandlers "github.com/router-for-me/CLIProxyAPI/v7/sdk/api/handlers"
	sdkopenai "github.com/router-for-me/CLIProxyAPI/v7/sdk/api/handlers/openai"

	coreusage "github.com/router-for-me/CLIProxyAPI/v7/sdk/cliproxy/usage"
	"github.com/router-for-me/CLIProxyAPI/v7/sdk/config"

	_ "github.com/router-for-me/CLIProxyAPI/v7/sdk/translator/builtin"
)

func runRelayHTTPServer(ctx context.Context, cfg *config.Config, handler http.Handler, emitter *eventEmitter) error {
	host := "127.0.0.1"
	port := 0
	if cfg != nil {
		if strings.TrimSpace(cfg.Host) != "" {
			host = strings.TrimSpace(cfg.Host)
		}
		port = cfg.Port
	}
	listener, err := net.Listen("tcp", net.JoinHostPort(host, strconv.Itoa(port)))
	if err != nil {
		return err
	}
	server := &http.Server{
		Handler:           handler,
		ReadHeaderTimeout: 30 * time.Second,
	}
	errCh := make(chan error, 1)
	go func() {
		if serveErr := server.Serve(listener); serveErr != nil && !errors.Is(serveErr, http.ErrServerClosed) {
			errCh <- serveErr
			return
		}
		errCh <- nil
	}()
	if emitter != nil {
		readyPort := port
		if tcpAddr, ok := listener.Addr().(*net.TCPAddr); ok {
			readyPort = tcpAddr.Port
		}
		emitter.emit(map[string]any{"type": "ready", "port": readyPort, "host": host})
	}
	select {
	case <-ctx.Done():
		shutdownCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		_ = server.Shutdown(shutdownCtx)
		return ctx.Err()
	case serveErr := <-errCh:
		return serveErr
	}
}

func monitorParentProcess(ctx context.Context, parentPID int, cancel context.CancelFunc, emitter *eventEmitter) {
	if parentPID <= 0 || parentPID == os.Getpid() {
		return
	}
	monitorParentProcessPlatform(ctx, parentPID, cancel, emitter)
}

func normalizeCodeRelayLocale(locale string) string {
	locale = strings.TrimSpace(locale)
	if locale == "" {
		return "en"
	}
	return locale
}

func main() {
	configPath := flag.String("config", "", "CLIProxyAPI config file")
	manifestPath := flag.String("manifest", "", "CodeRelay sidecar manifest file")
	quotaReserveStatePath := flag.String("quota-reserve-state", "", "CodeRelay OAuth quota reserve state file")
	quotaPoolStatePath := flag.String("quota-pool-state", "", "CodeRelay account-pool quota state file")
	parentPID := flag.Int("parent-pid", 0, "CodeRelay Tools parent process id")
	flag.Parse()

	emitter := &eventEmitter{}
	if strings.TrimSpace(*configPath) == "" || strings.TrimSpace(*manifestPath) == "" {
		emitter.emit(map[string]any{"type": "error", "message": "missing --config or --manifest"})
		os.Exit(2)
	}

	emitter.emitStartupStage("resolve_config_path")
	absConfigPath, err := filepath.Abs(*configPath)
	if err != nil {
		emitter.emit(map[string]any{"type": "error", "message": err.Error()})
		os.Exit(2)
	}
	emitter.emitStartupStage("load_config")
	cfg, err := config.LoadConfig(absConfigPath)
	if err != nil {
		emitter.emit(map[string]any{"type": "error", "message": err.Error()})
		os.Exit(2)
	}
	emitter.emitStartupStage("load_manifest")
	m, err := loadManifest(*manifestPath)
	if err != nil {
		emitter.emit(map[string]any{"type": "error", "message": err.Error()})
		os.Exit(2)
	}
	// CodeBuddy 模型清单优先从本地缓存加载（由「导入账号」或「手动同步」时
	// 持久化），避免启动后依赖异步后端同步导致的「未查询到可用模型」真空期。
	// 缓存缺失时回退到后端同步（在导入账号时触发）。
	emitter.emitStartupStage("load_codebuddy_model_cache")
	if cached, errCache := loadCodebuddyModelCache(codebuddyModelCachePath(*manifestPath)); errCache == nil {
		if ids := internalregistry.InstallCodebuddyModels(cached); len(ids) > 0 {
			m.setModelIDs(ids)
			emitter.emit(map[string]any{
				"type":    "codebuddy_model_cache_loaded",
				"message": fmt.Sprintf("codebuddy models loaded from cache (%d entries)", len(ids)),
			})
		}
	}
	emitter.emitStartupStage("init_runtime")
	quotaState := newQuotaReserveStateStore(*quotaReserveStatePath, m)
	if err := quotaState.load(); err != nil {
		emitter.emit(map[string]any{
			"type":    "quota_reserve_state_error",
			"message": err.Error(),
		})
	}

	usageTracker := newRequestUsageTracker()
	tokenLimiter := newAPIKeyTokenLimiter(m)
	policy := &requestPolicy{
		manifest:     m,
		emitter:      emitter,
		tracker:      usageTracker,
		tokenLimiter: tokenLimiter,
	}
	hook := &authHook{manifest: m, emitter: emitter}
	priorityState := newAPIKeyPriorityStateStore(*manifestPath)
	selector := &coderelaySelector{
		manifest:   m,
		emitter:    emitter,
		locale:     normalizeCodeRelayLocale(m.Locale),
		quota:      quotaState,
		priorities: priorityState,
		tracker:    usageTracker,
	}
	coreManager := buildCoreAuthManager(cfg, selector, hook, m, quotaState, usageTracker)

	signalCtx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	ctx, cancel := context.WithCancel(signalCtx)
	defer cancel()
	quotaState.start(ctx, emitter)
	monitorParentProcess(ctx, *parentPID, cancel, emitter)

	coreusage.RegisterPlugin(&usagePlugin{manifest: m, tracker: usageTracker})

	runtime, err := newSidecarRuntime(ctx, absConfigPath, cfg, m, coreManager)
	if err != nil {
		emitter.emit(map[string]any{"type": "error", "message": err.Error()})
		os.Exit(1)
	}
	defer runtime.Stop()
	// runCodebuddyModelSync 执行一次 CodeBuddy 模型同步：成功刷新清单并广播事件，
	// 失败写日志并发事件（Rust 侧落到 last_error，界面可见）。失败不动已有清单与
	// 本地缓存，避免把可用目录退化成只剩 auto。
	runCodebuddyModelSync := func(stage string) bool {
		synced, outcome := syncCodebuddyModelsFromBackend(coreManager.List(), codebuddyModelCachePath(*manifestPath))
		if len(synced) == 0 {
			log.Warnf("codebuddy model sync failed (%s): %s (accounts=%d attempts=%d httpStatus=%d bizCode=%d)",
				stage, outcome.Error, outcome.AccountsTried, outcome.Attempts, outcome.HTTPStatus, outcome.BizCode)
			emitter.emit(map[string]any{
				"type":    "codebuddy_model_sync_error",
				"message": "CodeBuddy 模型同步失败：" + outcome.Error,
			})
			return false
		}
		if !m.setModelIDs(synced) {
			log.Infof("codebuddy model sync ok (%s): %d models unchanged (account=%s)", stage, len(synced), outcome.AccountID)
			return true
		}
		internalregistry.NotifyCodebuddyModelRefresh()
		log.Infof("codebuddy model sync ok (%s): %d models via account=%s", stage, len(synced), outcome.AccountID)
		emitter.emit(map[string]any{
			"type":    "codebuddy_model_sync",
			"message": fmt.Sprintf("codebuddy models updated to %d entries (source=tencent-backend)", len(synced)),
		})
		return true
	}
	// 导入 codebuddy 账号时自动执行一次模型同步并持久化；不做定时自动拉取，
	// 以最大程度节省资源。手动「同步模型」按钮仍会覆盖本地缓存。
	hook.syncCodebuddy = func() { runCodebuddyModelSync("auth-hook") }
	// 服务启动后从 model 接口同步一次模型清单并覆盖本地缓存，使 /v1/models
	// 反映完整模型目录（后端接口直接拉取，不做任何本地 app.asar 提取）。
	// 异步执行，不阻塞 ready 事件与 HTTP 服务启动；失败则在启动窗口内补一次——
	// 账号池里存在失效 token 或网关偶发抖动时，单次失败不应让模型目录长期缺项。
	go func() {
		select {
		case <-time.After(500 * time.Millisecond):
		case <-ctx.Done():
			return
		}
		if runCodebuddyModelSync("startup") {
			return
		}
		select {
		case <-time.After(codebuddyModelSyncStartupRetryDelay):
		case <-ctx.Done():
			return
		}
		runCodebuddyModelSync("startup-retry")
	}()
	emitter.emitStartupStage("start_http_server")

	// Reuse the same coreManager so WS upgrades share OAuth pool, routing and
	// session affinity with POST /v1/responses.
	var sdkCfg *config.SDKConfig
	if cfg != nil {
		sdkCfg = &cfg.SDKConfig
	}
	baseHandlers := sdkhandlers.NewBaseAPIHandlers(sdkCfg, coreManager)
	responsesHandler := sdkopenai.NewOpenAIResponsesAPIHandler(baseHandlers)
	liveHandler := codexlive.NewHandler(coreManager, cfg)
	defer liveHandler.Close()
	relay := &relayServer{
		runtime:            runtime,
		cfg:                cfg,
		manifest:           m,
		manifestPath:       *manifestPath,
		authManager:        coreManager,
		emitter:            emitter,
		policy:             policy,
		responsesWebsocket: responsesHandler.ResponsesWebsocket,
		codexLive:          liveHandler,
		quotaPoolStatePath: *quotaPoolStatePath,
	}
	if err := runRelayHTTPServer(ctx, cfg, relay.router(), emitter); err != nil && !errors.Is(err, context.Canceled) {
		emitter.emit(map[string]any{"type": "error", "message": err.Error()})
		os.Exit(1)
	}
}
