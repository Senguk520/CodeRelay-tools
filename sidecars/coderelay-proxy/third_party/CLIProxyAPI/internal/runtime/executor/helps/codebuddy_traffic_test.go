package helps

import (
	"context"
	"fmt"
	"testing"
	"time"
)

func TestCodebuddyTrafficAllowsConfiguredBurst(t *testing.T) {
	profile := CodebuddyTrafficProfile{
		RequestsPerMinute: 600,
		MaxConcurrent:     2,
		MinIntervalMS:     0,
		JitterPercent:     0,
	}
	releaseFirst, err := AcquireCodebuddyTraffic(context.Background(), "traffic-burst", profile)
	if err != nil {
		t.Fatalf("first acquire: %v", err)
	}
	releaseSecond, err := AcquireCodebuddyTraffic(context.Background(), "traffic-burst", profile)
	if err != nil {
		releaseFirst()
		t.Fatalf("second acquire: %v", err)
	}
	releaseFirst()
	releaseSecond()
}

func TestCodebuddyTrafficCancellationDoesNotLeakSlot(t *testing.T) {
	profile := CodebuddyTrafficProfile{
		RequestsPerMinute: 1,
		MaxConcurrent:     1,
		MinIntervalMS:     1000,
		JitterPercent:     0,
	}
	release, err := AcquireCodebuddyTraffic(context.Background(), "traffic-cancel", profile)
	if err != nil {
		t.Fatalf("first acquire: %v", err)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Millisecond)
	defer cancel()
	if _, err := AcquireCodebuddyTraffic(ctx, "traffic-cancel", profile); err == nil {
		t.Fatal("expected cancellation while a slot is held")
	}
	release()

	releaseAgain, err := AcquireCodebuddyTraffic(context.Background(), "traffic-cancel", CodebuddyTrafficProfile{
		RequestsPerMinute: 600,
		MaxConcurrent:     1,
		MinIntervalMS:     0,
		JitterPercent:     0,
	})
	if err != nil {
		t.Fatalf("slot leaked after cancellation: %v", err)
	}
	releaseAgain()
}

func TestCodebuddyTrafficRegistryIsBounded(t *testing.T) {
	codebuddyTrafficLimiters.mu.Lock()
	codebuddyTrafficLimiters.entries = make(map[string]*codebuddyTrafficLimiter)
	codebuddyTrafficLimiters.mu.Unlock()

	profile := CodebuddyTrafficProfile{RequestsPerMinute: 6000, MaxConcurrent: 1, MinIntervalMS: 0, JitterPercent: 0}
	for index := 0; index < codebuddyTrafficMaxEntries+10; index++ {
		release, err := AcquireCodebuddyTraffic(context.Background(), fmt.Sprintf("bounded-%d", index), profile)
		if err != nil {
			t.Fatalf("acquire %d: %v", index, err)
		}
		release()
	}
	codebuddyTrafficLimiters.mu.Lock()
	got := len(codebuddyTrafficLimiters.entries)
	codebuddyTrafficLimiters.mu.Unlock()
	if got > codebuddyTrafficMaxEntries {
		t.Fatalf("registry grew to %d, want <= %d", got, codebuddyTrafficMaxEntries)
	}
}

func TestCodebuddyTrafficProfileDefaultsAreConservative(t *testing.T) {
	profile := normalizeCodebuddyTrafficProfile(CodebuddyTrafficProfile{})
	if profile.RequestsPerMinute <= 0 || profile.MaxConcurrent <= 0 || profile.MinIntervalMS < 0 {
		t.Fatalf("unsafe defaults: %#v", profile)
	}
	if profile.JitterPercent < 0 || profile.JitterPercent > 50 {
		t.Fatalf("jitter out of range: %#v", profile)
	}
}
