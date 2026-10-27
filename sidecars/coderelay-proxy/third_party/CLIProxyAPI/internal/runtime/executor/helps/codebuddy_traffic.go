package helps

import (
	"context"
	"fmt"
	"math"
	"math/rand"
	"sync"
	"time"
)

// CodebuddyTrafficProfile controls per-account pacing. Zero values are replaced
// by conservative defaults; callers should pass the persisted profile values.
type CodebuddyTrafficProfile struct {
	RequestsPerMinute int
	MaxConcurrent     int
	MinIntervalMS     int
	JitterPercent     int
}

type codebuddyTrafficLimiter struct {
	mu          sync.Mutex
	rate        float64
	burst       float64
	tokens      float64
	lastRefill  time.Time
	lastStart   time.Time
	minInterval time.Duration
	jitter      float64
	sem         chan struct{}
}

const codebuddyTrafficMaxEntries = 256

var codebuddyTrafficLimiters = struct {
	mu      sync.Mutex
	entries map[string]*codebuddyTrafficLimiter
}{entries: make(map[string]*codebuddyTrafficLimiter)}

// AcquireCodebuddyTraffic waits for a per-account token and concurrency slot.
// The returned release function must be called exactly once when the upstream
// request (including stream consumption) is finished.
func AcquireCodebuddyTraffic(ctx context.Context, accountID string, profile CodebuddyTrafficProfile) (func(), error) {
	profile = normalizeCodebuddyTrafficProfile(profile)
	key := fmt.Sprintf("%s|%d|%d|%d|%d", accountID, profile.RequestsPerMinute, profile.MaxConcurrent, profile.MinIntervalMS, profile.JitterPercent)
	codebuddyTrafficLimiters.mu.Lock()
	limiter := codebuddyTrafficLimiters.entries[key]
	if limiter == nil {
		if len(codebuddyTrafficLimiters.entries) >= codebuddyTrafficMaxEntries {
			for staleKey := range codebuddyTrafficLimiters.entries {
				delete(codebuddyTrafficLimiters.entries, staleKey)
				break
			}
		}
		limiter = newCodebuddyTrafficLimiter(profile)
		codebuddyTrafficLimiters.entries[key] = limiter
	}
	codebuddyTrafficLimiters.mu.Unlock()

	select {
	case limiter.sem <- struct{}{}:
	case <-ctx.Done():
		return nil, ctx.Err()
	}

	release := func() {
		<-limiter.sem
	}
	if err := limiter.wait(ctx); err != nil {
		limiter.refund()
		release()
		return nil, err
	}
	var once sync.Once
	return func() { once.Do(release) }, nil
}

func newCodebuddyTrafficLimiter(profile CodebuddyTrafficProfile) *codebuddyTrafficLimiter {
	return &codebuddyTrafficLimiter{
		rate:        float64(profile.RequestsPerMinute) / 60.0,
		burst:       math.Min(8, float64(profile.RequestsPerMinute)),
		tokens:      math.Min(8, float64(profile.RequestsPerMinute)),
		minInterval: time.Duration(profile.MinIntervalMS) * time.Millisecond,
		jitter:      float64(profile.JitterPercent) / 100.0,
		sem:         make(chan struct{}, profile.MaxConcurrent),
	}
}

func normalizeCodebuddyTrafficProfile(profile CodebuddyTrafficProfile) CodebuddyTrafficProfile {
	if profile.RequestsPerMinute <= 0 {
		profile.RequestsPerMinute = 240
	}
	if profile.RequestsPerMinute > 6000 {
		profile.RequestsPerMinute = 6000
	}
	if profile.MaxConcurrent <= 0 {
		profile.MaxConcurrent = 4
	}
	if profile.MaxConcurrent > 64 {
		profile.MaxConcurrent = 64
	}
	if profile.MinIntervalMS < 0 {
		profile.MinIntervalMS = 0
	}
	if profile.MinIntervalMS > 60_000 {
		profile.MinIntervalMS = 60_000
	}
	if profile.JitterPercent < 0 {
		profile.JitterPercent = 0
	}
	if profile.JitterPercent > 50 {
		profile.JitterPercent = 50
	}
	return profile
}

func (l *codebuddyTrafficLimiter) wait(ctx context.Context) error {
	for {
		l.mu.Lock()
		now := time.Now()
		if l.lastRefill.IsZero() {
			l.lastRefill = now
		}
		if elapsed := now.Sub(l.lastRefill).Seconds(); elapsed > 0 {
			l.tokens += elapsed * l.rate
			if l.tokens > l.burst {
				l.tokens = l.burst
			}
			l.lastRefill = now
		}
		if l.tokens >= 1 {
			l.tokens -= 1
			wait := time.Duration(0)
			if l.minInterval > 0 && !l.lastStart.IsZero() {
				next := l.lastStart.Add(l.minInterval)
				if now.Before(next) {
					wait = next.Sub(now)
					l.lastStart = next
				} else {
					l.lastStart = now
				}
			} else {
				l.lastStart = now
			}
			l.mu.Unlock()
			if wait > 0 {
				if err := sleepWithContext(ctx, wait); err != nil {
					return err
				}
			}
			return l.applyJitter(ctx)
		}
		wait := time.Duration((1 - l.tokens) / l.rate * float64(time.Second))
		l.mu.Unlock()
		if wait <= 0 {
			wait = time.Millisecond
		}
		if err := sleepWithContext(ctx, wait); err != nil {
			return err
		}
	}
}

func (l *codebuddyTrafficLimiter) applyJitter(ctx context.Context) error {
	if l.jitter <= 0 {
		return nil
	}
	base := l.minInterval
	if base <= 0 {
		base = 100 * time.Millisecond
	}
	maxJitter := time.Duration(float64(base) * l.jitter)
	if maxJitter <= 0 {
		return nil
	}
	return sleepWithContext(ctx, time.Duration(rand.Int63n(int64(maxJitter)+1)))
}

// refund returns one token after a cancelled wait so repeated cancellations do
// not permanently throttle the account.
func (l *codebuddyTrafficLimiter) refund() {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.tokens++
	if l.tokens > l.burst {
		l.tokens = l.burst
	}
}

func sleepWithContext(ctx context.Context, duration time.Duration) error {
	if duration <= 0 {
		return ctx.Err()
	}
	timer := time.NewTimer(duration)
	defer timer.Stop()
	select {
	case <-timer.C:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}
