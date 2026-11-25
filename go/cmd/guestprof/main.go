// Command guestprof shows where the Go SDK's wasm guest spends its time
// while it decrypts (or encrypts) 100 rows of 14 columns against ZeroKMS.
//
// It attaches a wazero function listener through the context given to
// encrypt.NewClient, and times the guest functions whose names match each
// -bucket pattern: inclusive time, counting only the outermost call of a
// bucket, so recursion and nesting within a bucket are not counted twice.
// It also reports each bucket's longest single call: for the "guest" bucket,
// the longest guest step.
// The guest's exports (se_*) always form the "guest" bucket: every guest
// step, so the total time in the guest.
//
// The guest must keep its name section. Build it with
// CARGO_PROFILE_RELEASE_STRIP=false and swap it into the SDK under test
// (scripts/go-variant.sh local), then:
//
//	guestprof -list 'recipher|sha2|aes_gcm'        # names to choose from
//	guestprof -bucket 'derive=from_key_material' -bucket 'aead=aes_gcm.*decrypt'
//
// A listener exits native code twice per call, so it slows what it times.
// Pick functions called a few times per value, not per block, and compare
// the "guest" total with a run that has no buckets.
package main

import (
	"context"
	"flag"
	"fmt"
	"log"
	"net/http"
	"os"
	"regexp"
	"runtime/pprof"
	"slices"
	"sort"
	"strings"
	"sync"
	"syscall"
	"time"

	"github.com/cipherstash/benches/go/model"
	"github.com/cipherstash/stack/languages/golang/encrypt"
	_ "github.com/cipherstash/stack/languages/golang/encrypt/eql"
	"github.com/tetratelabs/wazero/api"
	"github.com/tetratelabs/wazero/experimental"
)

type bucket struct {
	name  string
	re    *regexp.Regexp
	mu    sync.Mutex
	depth int
	start time.Time
	total time.Duration
	max   time.Duration
	calls int
	each  []time.Duration // every outermost call, for percentiles
}

type buckets []*bucket

func (b *buckets) String() string { return "" }

func (b *buckets) Set(v string) error {
	name, pattern, ok := strings.Cut(v, "=")
	if !ok {
		return fmt.Errorf("-bucket wants name=regexp, got %q", v)
	}
	re, err := regexp.Compile(pattern)
	if err != nil {
		return err
	}
	*b = append(*b, &bucket{name: name, re: re})
	return nil
}

type listener struct{ b *bucket }

func (l listener) Before(context.Context, api.Module, api.FunctionDefinition, []uint64, experimental.StackIterator) {
	l.b.mu.Lock()
	if l.b.depth == 0 {
		l.b.start = time.Now()
	}
	l.b.depth++
	l.b.mu.Unlock()
}

func (l listener) After(context.Context, api.Module, api.FunctionDefinition, []uint64) {
	l.b.mu.Lock()
	l.b.depth--
	if l.b.depth == 0 {
		d := time.Since(l.b.start)
		l.b.each = append(l.b.each, d)
		l.b.total += d
		l.b.max = max(l.b.max, d)
		l.b.calls++
	}
	l.b.mu.Unlock()
}

func (l listener) Abort(ctx context.Context, m api.Module, d api.FunctionDefinition, _ error) {
	l.After(ctx, m, d, nil)
}

type factory struct {
	list    *regexp.Regexp
	buckets []*bucket
	listed  map[string]bool
}

func (f *factory) NewFunctionListener(def api.FunctionDefinition) experimental.FunctionListener {
	name := def.DebugName()
	if f.list != nil {
		if f.list.MatchString(name) && !f.listed[name] {
			f.listed[name] = true
			fmt.Println(name)
		}
		return nil
	}
	for _, b := range f.buckets {
		if b.re.MatchString(name) {
			return listener{b}
		}
	}
	return nil
}

func cpuNow() time.Duration {
	var ru syscall.Rusage
	_ = syscall.Getrusage(syscall.RUSAGE_SELF, &ru)
	return time.Duration(ru.Utime.Nano() + ru.Stime.Nano())
}

func main() {
	var bs buckets
	list := flag.String("list", "", "print the guest functions whose names match this and exit")
	flag.Var(&bs, "bucket", "name=regexp: time the guest functions matching regexp as one bucket (repeatable)")
	op := flag.String("op", "decrypt", "decrypt | encrypt")
	calls := flag.Int("calls", 10, "calls to time, after one warm-up")
	rows := flag.Int("rows", 100, "rows of 14 columns per call")
	cpuprofile := flag.String("cpuprofile", "", "write a Go CPU profile of the timed calls here")
	flag.Parse()

	guest := &bucket{name: "guest (all se_* exports)", re: regexp.MustCompile(`\.se_[a-z_]+$`)}
	f := &factory{buckets: append(buckets{guest}, bs...), listed: map[string]bool{}}
	if *list != "" {
		f.list = regexp.MustCompile(*list)
	}
	ctx := experimental.WithFunctionListenerFactory(context.Background(), f)
	arrivals := &arrivalLog{next: http.DefaultTransport}
	client, err := encrypt.NewClient(ctx, encrypt.WithTransport(arrivals))
	if err != nil {
		log.Fatal(err)
	}
	defer client.Close()
	if f.list != nil {
		return
	}
	cipher := client.DefaultKeyset()
	run := context.Background()

	r := model.NewRand(42)
	people := make([]model.Individual, *rows)
	for i := range people {
		people[i] = model.GenIndividual(r, int64(i+1))
	}
	sealed, err := model.EncryptIndividuals(run, cipher, people)
	if err != nil {
		log.Fatal(err)
	}
	call := func() error {
		if *op == "encrypt" {
			_, err := model.EncryptIndividuals(run, cipher, people)
			return err
		}
		_, err := model.DecryptIndividuals(run, cipher, sealed)
		return err
	}
	if err := call(); err != nil { // warm up
		log.Fatal(err)
	}
	for _, b := range f.buckets {
		b.total, b.max, b.calls, b.each = 0, 0, 0, nil
	}
	if *cpuprofile != "" {
		out, err := os.Create(*cpuprofile)
		if err != nil {
			log.Fatal(err)
		}
		if err := pprof.StartCPUProfile(out); err != nil {
			log.Fatal(err)
		}
		defer pprof.StopCPUProfile()
	}
	arrivals.reset()
	c0, t0 := cpuNow(), time.Now()
	for range *calls {
		if err := call(); err != nil {
			log.Fatal(err)
		}
	}
	cpu, wall := cpuNow()-c0, time.Since(t0)
	n := float64(*calls)
	values := 14.0 * float64(*rows)
	fmt.Printf("%s of %d rows x 14 columns, %d calls: process CPU %.1f ms/call, wall %.1f ms/call\n",
		*op, *rows, *calls, float64(cpu.Microseconds())/1000/n, float64(wall.Microseconds())/1000/n)
	reportSteps(guest, arrivals, *calls)
	sort.SliceStable(f.buckets, func(i, j int) bool { return f.buckets[i].total > f.buckets[j].total })
	fmt.Printf("%-34s %10s %10s %12s %8s %12s\n", "bucket", "ms/call", "µs/value", "calls/call", "% CPU", "longest (ms)")
	for _, b := range f.buckets {
		ms := float64(b.total.Microseconds()) / 1000 / n
		fmt.Printf("%-34s %10.2f %10.1f %12.0f %7.0f%% %12.2f\n", b.name, ms, ms*1000/values, float64(b.calls)/n,
			100*float64(b.total)/float64(cpu), float64(b.max.Microseconds())/1000)
	}
}

// arrivalLog records when each ZeroKMS data-key request arrives. A step's
// requests are sent together, microseconds apart; steps are a round trip
// apart. So a gap of more than 5 ms starts a new step.
type arrivalLog struct {
	next http.RoundTripper
	mu   sync.Mutex
	at   []time.Time
}

func (a *arrivalLog) RoundTrip(r *http.Request) (*http.Response, error) {
	if strings.Contains(r.URL.Path, "data-key") {
		a.mu.Lock()
		a.at = append(a.at, time.Now())
		a.mu.Unlock()
	}
	return a.next.RoundTrip(r)
}

func (a *arrivalLog) reset() {
	a.mu.Lock()
	a.at = nil
	a.mu.Unlock()
}

func (a *arrivalLog) groups() []int {
	a.mu.Lock()
	defer a.mu.Unlock()
	var out []int
	for i, t := range a.at {
		if i == 0 || t.Sub(a.at[i-1]) > 5*time.Millisecond {
			out = append(out, 0)
		}
		out[len(out)-1]++
	}
	return out
}

// reportSteps prints the steps per call: how many send requests and how
// many each sends, the rest (yields and the last step), and the longest
// and 99th percentile step.
func reportSteps(guest *bucket, a *arrivalLog, calls int) {
	g := a.groups()
	hist := map[int]int{}
	for _, n := range g {
		hist[n]++
	}
	keys := make([]int, 0, len(hist))
	for k := range hist {
		keys = append(keys, k)
	}
	sort.Ints(keys)
	var h []string
	for _, k := range keys {
		h = append(h, fmt.Sprintf("%d req x %.1f", k, float64(hist[k])/float64(calls)))
	}
	each := slices.Clone(guest.each)
	slices.Sort(each)
	p := func(q float64) float64 {
		if len(each) == 0 {
			return 0
		}
		return float64(each[min(len(each)-1, int(q*float64(len(each))))].Microseconds()) / 1000
	}
	n := float64(calls)
	fmt.Printf("steps/call %.1f: request steps/call %.1f (%s), other steps/call %.1f; step ms: p50 %.2f p99 %.2f max %.2f\n",
		float64(len(guest.each))/n, float64(len(g))/n, strings.Join(h, ", "),
		(float64(len(guest.each))-float64(len(g)))/n, p(0.5), p(0.99), p(1))
}
