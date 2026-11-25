// Package harness runs the Go SDK bench cases and records them.
package harness

import (
	"context"
	"encoding/json"
	"fmt"
	"log"
	"os"
	"sort"
	"syscall"
	"time"

	"github.com/cipherstash/benches/go/model"
	"github.com/cipherstash/stack/languages/golang/encrypt"
	_ "github.com/cipherstash/stack/languages/golang/encrypt/eql"
)

// Result is one case: one JSON line on -out.
type Result struct {
	SDK      string    `json:"sdk"`
	Variant  string    `json:"variant"`
	Mode     string    `json:"mode"`
	Tier     int       `json:"tier,omitempty"`
	Scenario string    `json:"scenario"`
	Op       string    `json:"op"`
	Rows     int       `json:"rows"`
	Values   int       `json:"values"`
	Clients  int       `json:"clients,omitempty"`
	WallMS   []float64 `json:"wall_ms"`
	CPUMS    []float64 `json:"cpu_ms"`
	QueryMS  []float64 `json:"query_ms,omitempty"`
	At       string    `json:"at"`
}

func cpuNow() time.Duration {
	var ru syscall.Rusage
	if err := syscall.Getrusage(syscall.RUSAGE_SELF, &ru); err != nil {
		panic(err)
	}
	return time.Duration(ru.Utime.Nano() + ru.Stime.Nano())
}

func ms(d time.Duration) float64 { return float64(d.Microseconds()) / 1000 }

// measure runs f calls times and records wall and CPU per call.
func measure(calls int, f func() error) (wall, cpu []float64, err error) {
	for i := 0; i < calls; i++ {
		c0, t0 := cpuNow(), time.Now()
		if err = f(); err != nil {
			return
		}
		wall = append(wall, ms(time.Since(t0)))
		cpu = append(cpu, ms(cpuNow()-c0))
	}
	return
}

func median(xs []float64) float64 {
	if len(xs) == 0 {
		return 0
	}
	s := append([]float64(nil), xs...)
	sort.Float64s(s)
	return s[len(s)/2]
}

func minmax(xs []float64) (float64, float64) {
	if len(xs) == 0 {
		return 0, 0
	}
	lo, hi := xs[0], xs[0]
	for _, x := range xs {
		lo, hi = min(lo, x), max(hi, x)
	}
	return lo, hi
}

var (
	out *json.Encoder
	// Variant labels every result: which guest and host build ran.
	Variant = "shipped"
)

// SetOut appends every result to path as a JSON line.
func SetOut(path string) (close func() error, err error) {
	f, err := os.OpenFile(path, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o644)
	if err != nil {
		return nil, err
	}
	out = json.NewEncoder(f)
	return f.Close, nil
}

func emit(r Result) {
	r.SDK, r.Variant, r.At = "go", Variant, time.Now().UTC().Format(time.RFC3339)
	if out != nil {
		if err := out.Encode(r); err != nil {
			log.Fatal(err)
		}
	}
	lo, hi := minmax(r.CPUMS)
	perValue := median(r.CPUMS) / float64(max(r.Values, 1)) * 1000
	q := ""
	if len(r.QueryMS) > 0 {
		q = fmt.Sprintf("  query %7.2f ms", median(r.QueryMS))
	}
	fmt.Printf("%-8s %-22s %-8s rows=%-4d vals=%-5d cpu %8.2f ms (%.2f–%.2f)  wall %8.2f ms  %6.1f µs/value%s\n",
		r.Variant, r.Scenario, r.Op, r.Rows, r.Values, median(r.CPUMS), lo, hi, median(r.WallMS), perValue, q)
}

// RunMicro is BNDRY's table: encrypt the inputs once, then decrypt them
// rounds x calls times. Encrypt is timed the same way, for reference.
func RunMicro(ctx context.Context, cipher *encrypt.Cipher, mode string, rounds, calls int, only string) {
	r := model.NewRand(42)
	people := make([]model.Individual, 100)
	short := make([]model.ShortText, 100)
	long := make([]model.LongText, 100)
	docs := make([]model.JSONDoc, 100)
	for i := range 100 {
		people[i] = model.GenIndividual(r, int64(i+1))
		short[i] = model.ShortText{ID: int64(i + 1), FirstName: people[i].FirstName}
		long[i] = model.LongText{ID: int64(i + 1), Bio: model.GenText(r, 2048)}
		docs[i] = model.JSONDoc{ID: int64(i + 1), Profile: model.GenJSON(r, int64(i+1), 2048)}
	}

	type kase struct {
		name         string
		rows, values int
		enc, dec     func() error
	}
	var (
		encShort1, encShort []model.EncryptedShortText
		encLong             []model.EncryptedLongText
		encDocs             []model.EncryptedJSONDoc
		encPeople           []model.EncryptedIndividual
	)
	must := func(err error) {
		if err != nil {
			log.Fatal(err)
		}
	}
	var err error
	encShort1, err = model.EncryptShortTexts(ctx, cipher, short[:1])
	must(err)
	encShort, err = model.EncryptShortTexts(ctx, cipher, short)
	must(err)
	encLong, err = model.EncryptLongTexts(ctx, cipher, long)
	must(err)
	encDocs, err = model.EncryptJSONDocs(ctx, cipher, docs)
	must(err)
	encPeople, err = model.EncryptIndividuals(ctx, cipher, people)
	must(err)

	cases := []kase{
		{"1_row_short_text", 1, 1,
			func() error { _, err := model.EncryptShortTexts(ctx, cipher, short[:1]); return err },
			func() error { _, err := model.DecryptShortTexts(ctx, cipher, encShort1); return err }},
		{"100_rows_short_text", 100, 100,
			func() error { _, err := model.EncryptShortTexts(ctx, cipher, short); return err },
			func() error { _, err := model.DecryptShortTexts(ctx, cipher, encShort); return err }},
		{"100_rows_2kb_text", 100, 100,
			func() error { _, err := model.EncryptLongTexts(ctx, cipher, long); return err },
			func() error { _, err := model.DecryptLongTexts(ctx, cipher, encLong); return err }},
		{"100_rows_2kb_json", 100, 100,
			func() error { _, err := model.EncryptJSONDocs(ctx, cipher, docs); return err },
			func() error { _, err := model.DecryptJSONDocs(ctx, cipher, encDocs); return err }},
		{"100_rows_14_cols", 100, 1400,
			func() error { _, err := model.EncryptIndividuals(ctx, cipher, people); return err },
			func() error { _, err := model.DecryptIndividuals(ctx, cipher, encPeople); return err }},
	}
	for _, k := range cases {
		if only != "" && k.name != only {
			continue
		}
		for _, op := range []struct {
			name string
			f    func() error
		}{{"decrypt", k.dec}, {"encrypt", k.enc}} {
			must(op.f()) // warm up: keyset load, first-call compilation
			var wall, cpu []float64
			for range rounds {
				w, c, err := measure(calls, op.f)
				must(err)
				wall, cpu = append(wall, w...), append(cpu, c...)
			}
			emit(Result{Mode: mode, Scenario: k.name, Op: op.name, Rows: k.rows, Values: k.values, WallMS: wall, CPUMS: cpu})
		}
	}
}
