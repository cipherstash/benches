package offline

import (
	"context"
	"fmt"
	"math"
	"runtime"
	"runtime/metrics"
	"sync"
	"testing"
	"time"

	"github.com/cipherstash/benches/go/model"
	"github.com/cipherstash/stack/languages/golang/encrypt/encrypttest"
)

// TestGCLatency measures what a running guest call does to the rest of the
// process. wazero's native code cannot be preempted by the Go scheduler.
// With WithCloseOnContextDone, the guest exits to Go at every loop header,
// and each exit is a point where the goroutine can stop for the garbage
// collector. Without the option, a stop-the-world pause must wait for the
// guest call to return.
//
// One goroutine decrypts 100 rows of 14 columns in a loop. Meanwhile the
// test forces a GC every 5 ms and records the stop-the-world pauses
// (runtime/metrics /sched/pauses/total/gc:seconds) and how late a 1 ms
// ticker fires (scheduling latency for an unrelated goroutine).
//
//	go test ./offline -run TestGCLatency -v -args -variant noclose
func TestGCLatency(t *testing.T) {
	ctx := context.Background()
	client := encrypttest.NewClient(t)
	cipher := client.DefaultKeyset()
	r := model.NewRand(42)
	people := make([]model.Individual, 100)
	for i := range people {
		people[i] = model.GenIndividual(r, int64(i+1))
	}
	sealed, err := model.EncryptIndividuals(ctx, cipher, people)
	if err != nil {
		t.Fatal(err)
	}
	// Warm up, and time one decrypt on its own.
	t0 := time.Now()
	if _, err := model.DecryptIndividuals(ctx, cipher, sealed); err != nil {
		t.Fatal(err)
	}
	one := time.Since(t0)

	pausesBefore := gcPauses()
	stop := make(chan struct{})
	var wg sync.WaitGroup
	wg.Add(1)
	calls := 0
	go func() {
		defer wg.Done()
		for {
			select {
			case <-stop:
				return
			default:
			}
			if _, err := model.DecryptIndividuals(ctx, cipher, sealed); err != nil {
				t.Error(err)
				return
			}
			calls++
		}
	}()

	// An unrelated goroutine that wants to run every millisecond.
	var lateMax, lateSum time.Duration
	var ticks int
	wg.Add(1)
	go func() {
		defer wg.Done()
		next := time.Now().Add(time.Millisecond)
		for {
			select {
			case <-stop:
				return
			default:
			}
			time.Sleep(time.Until(next))
			late := time.Since(next)
			lateSum += late
			lateMax = max(lateMax, late)
			ticks++
			next = next.Add(time.Millisecond)
			if time.Until(next) < 0 {
				next = time.Now().Add(time.Millisecond)
			}
		}
	}()

	gcs := 0
	deadline := time.Now().Add(5 * time.Second)
	for time.Now().Before(deadline) {
		runtime.GC()
		gcs++
		time.Sleep(5 * time.Millisecond)
	}
	close(stop)
	wg.Wait()

	p50, p99, pmax := gcPauses().since(pausesBefore)
	line := fmt.Sprintf("%-10s one decrypt %6.1f ms | %4d decrypts, %4d GCs | STW pause p50 %7.3f ms  p99 %7.3f ms  max %7.3f ms | ticker late avg %6.3f ms  max %7.3f ms",
		*variant, ms(one), calls, gcs, p50, p99, pmax, ms(lateSum/time.Duration(max(ticks, 1))), ms(lateMax))
	fmt.Println(line)
}

func ms(d time.Duration) float64 { return float64(d) / float64(time.Millisecond) }

type pauseHist struct {
	counts  []uint64
	buckets []float64
}

func gcPauses() pauseHist {
	s := []metrics.Sample{{Name: "/sched/pauses/total/gc:seconds"}}
	metrics.Read(s)
	h := s[0].Value.Float64Histogram()
	return pauseHist{counts: append([]uint64(nil), h.Counts...), buckets: h.Buckets}
}

// since gives the p50, p99 and max of the pauses recorded after before, in
// ms, from the histogram's bucket upper bounds.
func (h pauseHist) since(before pauseHist) (p50, p99, pmax float64) {
	var total uint64
	d := make([]uint64, len(h.counts))
	for i := range h.counts {
		d[i] = h.counts[i] - before.counts[i]
		total += d[i]
	}
	if total == 0 {
		return
	}
	upper := func(i int) float64 {
		b := h.buckets[i+1]
		if math.IsInf(b, 1) {
			b = h.buckets[i]
		}
		return b * 1000
	}
	var run uint64
	for i, c := range d {
		if c == 0 {
			continue
		}
		run += c
		if p50 == 0 && run*2 >= total {
			p50 = upper(i)
		}
		if p99 == 0 && run*100 >= total*99 {
			p99 = upper(i)
		}
		pmax = upper(i)
	}
	return
}
