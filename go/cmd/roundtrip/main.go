// Command roundtrip checks that values one build of the Go SDK encrypts
// decrypt under another, against real ZeroKMS (verification of
// cipherstash/stack#1773).
//
//	roundtrip -mode encrypt -out f.json [-seed N] [-sizes 1,499,...] [-keysets a,b,...]
//	roundtrip -mode decrypt -in f.json
//
// Plaintexts are generated from the seed and the keyset, so the file holds
// only the seed and the ciphertexts. Each size is run twice: as that many
// one-column ShortText values, and as ceil(size/14) 14-column Individuals,
// whose batch mixes text, match, ORE and date columns. With -keysets, every
// size is encrypted under the default keyset and under each keyset named.
//
// A decrypt checks every value of every case:
//   - as one batch through the cipher of the case's keyset, and through the
//     client, which opens each record under the keyset that sealed it;
//   - record by record, through the case's cipher, on -workers goroutines;
//   - with more than one keyset, it checks that another keyset's cipher
//     refuses the case, and decrypts every keyset's case of each kind and
//     size as one interleaved batch through a new client, whose keysets are
//     not cached yet. It reports the most ZeroKMS requests in flight at once
//     during that batch: one batch's requests run only within a step.
package main

import (
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"hash/fnv"
	"log"
	"net/http"
	"os"
	"runtime/debug"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/cipherstash/benches/go/model"
	"github.com/cipherstash/stack/languages/golang/encrypt"
	_ "github.com/cipherstash/stack/languages/golang/encrypt/eql"
)

type file struct {
	Build   string     `json:"build"`
	Seed    uint64     `json:"seed"`
	Keysets []string   `json:"keysets,omitempty"`
	Cases   []fileCase `json:"cases"`
}

type fileCase struct {
	Kind        string                      `json:"kind"`
	Size        int                         `json:"size"`
	Keyset      string                      `json:"keyset,omitempty"` // "" is the default keyset
	ShortTexts  []model.EncryptedShortText  `json:"short_texts,omitempty"`
	Individuals []model.EncryptedIndividual `json:"individuals,omitempty"`
}

// keysetSeed gives each keyset its own plaintexts.
func keysetSeed(seed uint64, keyset string) uint64 {
	if keyset == "" {
		return seed
	}
	h := fnv.New64a()
	h.Write([]byte(keyset))
	return seed ^ h.Sum64()
}

func shortTexts(seed uint64, keyset string, size int) []model.ShortText {
	r := model.NewRand(keysetSeed(seed, keyset) ^ uint64(size)*0x100000001b3)
	out := make([]model.ShortText, size)
	for k := range out {
		ind := model.GenIndividual(r, int64(k))
		out[k] = model.ShortText{ID: int64(k), FirstName: fmt.Sprintf("%s-%s-%d", ind.FirstName, keyset, k)}
	}
	return out
}

func individuals(seed uint64, keyset string, size int) []model.Individual {
	r := model.NewRand(keysetSeed(seed, keyset) ^ uint64(size)*0xcbf29ce484222325)
	out := make([]model.Individual, (size+13)/14)
	for k := range out {
		out[k] = model.GenIndividual(r, int64(k))
	}
	return out
}

func build() string {
	bi, ok := debug.ReadBuildInfo()
	if !ok {
		return "unknown"
	}
	for _, d := range bi.Deps {
		if d.Path == "github.com/cipherstash/stack/languages/golang" {
			if d.Replace != nil {
				return "local:" + d.Replace.Path
			}
			return d.Version
		}
	}
	return "unknown"
}

// clients builds clients that log failed requests when asked, and count
// requests in flight.
type clients struct {
	logHTTP bool
	flight  *inFlight
}

func (cs clients) new(ctx context.Context) *encrypt.Client {
	var rt http.RoundTripper = http.DefaultTransport
	if cs.logHTTP {
		rt = errLog{rt}
	}
	rt = &inFlightRT{next: rt, f: cs.flight}
	client, err := encrypt.NewClient(ctx, encrypt.WithTransport(rt))
	if err != nil {
		log.Fatalf("NewClient: %v", err)
	}
	return client
}

func cipherFor(c *encrypt.Client, keyset string) *encrypt.Cipher {
	if keyset == "" {
		return c.DefaultKeyset()
	}
	return c.Keyset(encrypt.KeysetName(keyset))
}

func ksName(k string) string {
	if k == "" {
		return "default"
	}
	return k
}

func main() {
	mode := flag.String("mode", "", "encrypt | decrypt")
	out := flag.String("out", "", "encrypt: write here")
	in := flag.String("in", "", "decrypt: read this")
	seed := flag.Uint64("seed", uint64(time.Now().UnixNano()), "plaintext seed")
	sizesFlag := flag.String("sizes", "1,499,500,501,1499,1500,1501,2501,10000", "batch sizes in values")
	keysetsFlag := flag.String("keysets", "", "encrypt: keyset names to encrypt under as well as the default, comma-separated")
	workers := flag.Int("workers", 16, "decrypt: concurrent record-by-record decrypts")
	onlyKind := flag.String("only-kind", "", "decrypt: only this kind")
	onlySize := flag.Int("only-size", 0, "decrypt: only this size")
	logHTTP := flag.Bool("log-http", false, "log every failed HTTP request")
	flag.Parse()

	ctx := context.Background()
	cs := clients{logHTTP: *logHTTP, flight: &inFlight{}}
	client := cs.new(ctx)
	defer client.Close()
	log.Printf("build %s", build())

	switch *mode {
	case "encrypt":
		keysets := []string{""}
		if *keysetsFlag != "" {
			keysets = append(keysets, strings.Split(*keysetsFlag, ",")...)
		}
		f := file{Build: build(), Seed: *seed, Keysets: keysets}
		for _, ks := range keysets {
			cipher := cipherFor(client, ks)
			for _, s := range strings.Split(*sizesFlag, ",") {
				size, err := strconv.Atoi(s)
				if err != nil {
					log.Fatal(err)
				}
				st, err := model.EncryptShortTexts(ctx, cipher, shortTexts(*seed, ks, size))
				if err != nil {
					log.Fatalf("encrypt %d short texts under %s: %v", size, ksName(ks), err)
				}
				f.Cases = append(f.Cases, fileCase{Kind: "short_text", Size: size, Keyset: ks, ShortTexts: st})
				ind, err := model.EncryptIndividuals(ctx, cipher, individuals(*seed, ks, size))
				if err != nil {
					log.Fatalf("encrypt %d individuals under %s: %v", size, ksName(ks), err)
				}
				f.Cases = append(f.Cases, fileCase{Kind: "individual", Size: size, Keyset: ks, Individuals: ind})
				log.Printf("encrypted size %d under %s: %d short texts, %d individuals (%d values)", size, ksName(ks), len(st), len(ind), 14*len(ind))
			}
		}
		// The encrypts run one after another, so this is the most requests
		// one step sent, the first use of each keyset included.
		log.Printf("encrypt: %d ZeroKMS requests, at most %d in flight at once", cs.flight.total.Load(), cs.flight.max.Load())
		b, err := json.Marshal(f)
		if err != nil {
			log.Fatal(err)
		}
		if err := os.WriteFile(*out, b, 0o600); err != nil {
			log.Fatal(err)
		}
	case "decrypt":
		b, err := os.ReadFile(*in)
		if err != nil {
			log.Fatal(err)
		}
		var f file
		if err := json.Unmarshal(b, &f); err != nil {
			log.Fatal(err)
		}
		failed := false
		report := func(c fileCase, check string, values int, ok bool, extra string) {
			status := "PASS"
			if !ok {
				status, failed = "FAIL", true
			}
			// One line per check, parsed into the report.
			fmt.Printf("RESULT\tenc=%s\tdec=%s\tkind=%s\tsize=%d\tkeyset=%s\tcheck=%s\tvalues=%d\t%s%s\n",
				f.Build, build(), c.Kind, c.Size, ksName(c.Keyset), check, values, status, extra)
		}
		var keep []fileCase
		for _, c := range f.Cases {
			if (*onlyKind != "" && c.Kind != *onlyKind) || (*onlySize != 0 && c.Size != *onlySize) {
				continue
			}
			keep = append(keep, c)
		}
		for _, c := range keep {
			cipher := cipherFor(client, c.Keyset)
			values, ok := check(ctx, cipher, f.Seed, c, "cipher batch")
			report(c, "cipher-batch", values, ok, "")
			values, ok = check(ctx, client, f.Seed, c, "client batch")
			report(c, "client-batch", values, ok, "")
			values, ok = checkEach(ctx, cipher, f.Seed, c, *workers)
			report(c, "cipher-each", values, ok, "")
		}
		if len(f.Keysets) > 1 {
			// Another keyset's cipher refuses a case, and never opens it.
			for _, c := range keep {
				other := f.Keysets[0]
				for i, ks := range f.Keysets {
					if ks == c.Keyset {
						other = f.Keysets[(i+1)%len(f.Keysets)]
					}
				}
				values, refused := refuses(ctx, cipherFor(client, other), c)
				report(c, "refused-by-"+ksName(other), values, refused, "")
			}
			// Every keyset's case of a kind and size, interleaved into one
			// batch, through a client whose keysets are not cached.
			type key struct {
				kind string
				size int
			}
			groups := map[key][]fileCase{}
			var order []key
			for _, c := range keep {
				k := key{c.Kind, c.Size}
				if _, ok := groups[k]; !ok {
					order = append(order, k)
				}
				groups[k] = append(groups[k], c)
			}
			for _, k := range order {
				fresh := cs.new(ctx)
				cs.flight.reset()
				values, ok := checkMixed(ctx, fresh, f.Seed, groups[k])
				fresh.Close()
				mixed := fileCase{Kind: k.kind, Size: k.size, Keyset: fmt.Sprintf("%d-keysets-mixed", len(groups[k]))}
				report(mixed, "client-mixed-batch", values, ok, fmt.Sprintf("\tmax_in_flight=%d\trequests=%d", cs.flight.max.Load(), cs.flight.total.Load()))
			}
		}
		if failed {
			os.Exit(1)
		}
	default:
		log.Fatalf("-mode encrypt|decrypt")
	}
}

// check decrypts a case as one batch through d and compares every record
// with its plaintext.
func check(ctx context.Context, d encrypt.Decrypter, seed uint64, c fileCase, what string) (values int, ok bool) {
	switch c.Kind {
	case "short_text":
		want := shortTexts(seed, c.Keyset, c.Size)
		got, err := model.DecryptShortTexts(ctx, d, c.ShortTexts)
		return len(want), same(c, what, got, want, err)
	case "individual":
		want := individuals(seed, c.Keyset, c.Size)
		got, err := model.DecryptIndividuals(ctx, d, c.Individuals)
		return 14 * len(want), same(c, what, got, want, err)
	}
	return 0, false
}

func same[T comparable](c fileCase, what string, got, want []T, err error) bool {
	if err != nil {
		log.Printf("%s %d %s %s: %v", c.Kind, c.Size, ksName(c.Keyset), what, err)
		return false
	}
	if len(got) != len(want) {
		log.Printf("%s %d %s %s: %d records, want %d", c.Kind, c.Size, ksName(c.Keyset), what, len(got), len(want))
		return false
	}
	for k := range got {
		if got[k] != want[k] {
			log.Printf("%s %d %s %s: record %d differs", c.Kind, c.Size, ksName(c.Keyset), what, k)
			return false
		}
	}
	return true
}

// checkEach decrypts a case record by record on workers goroutines.
func checkEach(ctx context.Context, cipher *encrypt.Cipher, seed uint64, c fileCase, workers int) (values int, ok bool) {
	switch c.Kind {
	case "short_text":
		want := shortTexts(seed, c.Keyset, c.Size)
		return len(want), each(len(want), workers, func(k int) bool {
			g, err := model.DecryptShortTexts(ctx, cipher, c.ShortTexts[k:k+1])
			if err != nil {
				log.Printf("%s %d %s record %d: %v", c.Kind, c.Size, ksName(c.Keyset), k, err)
				return false
			}
			return len(g) == 1 && g[0] == want[k]
		})
	case "individual":
		want := individuals(seed, c.Keyset, c.Size)
		return 14 * len(want), each(len(want), workers, func(k int) bool {
			g, err := model.DecryptIndividuals(ctx, cipher, c.Individuals[k:k+1])
			if err != nil {
				log.Printf("%s %d %s record %d: %v", c.Kind, c.Size, ksName(c.Keyset), k, err)
				return false
			}
			return len(g) == 1 && g[0] == want[k]
		})
	}
	return 0, false
}

// refuses reports whether cipher, of another keyset, refuses the case.
func refuses(ctx context.Context, cipher *encrypt.Cipher, c fileCase) (values int, refused bool) {
	var err error
	switch c.Kind {
	case "short_text":
		values = len(c.ShortTexts)
		_, err = model.DecryptShortTexts(ctx, cipher, c.ShortTexts)
	case "individual":
		values = 14 * len(c.Individuals)
		_, err = model.DecryptIndividuals(ctx, cipher, c.Individuals)
	}
	if err == nil {
		log.Printf("%s %d %s: another keyset's cipher opened it", c.Kind, c.Size, ksName(c.Keyset))
	}
	return values, err != nil
}

// interleave takes the k-th record of each case in turn, so neighbouring
// records of the batch come from different keysets.
func interleave[T any](cases [][]T) []T {
	var out []T
	for k := 0; ; k++ {
		added := false
		for _, c := range cases {
			if k < len(c) {
				out = append(out, c[k])
				added = true
			}
		}
		if !added {
			return out
		}
	}
}

// checkMixed decrypts every case in cases, interleaved into one batch,
// through client, and compares each record with its own plaintext.
func checkMixed(ctx context.Context, client *encrypt.Client, seed uint64, cases []fileCase) (values int, ok bool) {
	mixed := fileCase{Kind: cases[0].Kind, Size: cases[0].Size, Keyset: "mixed"}
	switch cases[0].Kind {
	case "short_text":
		var sealed [][]model.EncryptedShortText
		var wants [][]model.ShortText
		for _, c := range cases {
			sealed = append(sealed, c.ShortTexts)
			wants = append(wants, shortTexts(seed, c.Keyset, c.Size))
		}
		want := interleave(wants)
		got, err := model.DecryptShortTexts(ctx, client, interleave(sealed))
		return len(want), same(mixed, "client mixed batch", got, want, err)
	case "individual":
		var sealed [][]model.EncryptedIndividual
		var wants [][]model.Individual
		for _, c := range cases {
			sealed = append(sealed, c.Individuals)
			wants = append(wants, individuals(seed, c.Keyset, c.Size))
		}
		want := interleave(wants)
		got, err := model.DecryptIndividuals(ctx, client, interleave(sealed))
		return 14 * len(want), same(mixed, "client mixed batch", got, want, err)
	}
	return 0, false
}

// each runs f for 0..n-1 on workers goroutines and reports whether every
// call returned true.
func each(n, workers int, f func(int) bool) bool {
	var next atomic.Int64
	var bad atomic.Int64
	var wg sync.WaitGroup
	for range workers {
		wg.Go(func() {
			for {
				k := int(next.Add(1) - 1)
				if k >= n {
					return
				}
				if !f(k) {
					bad.Add(1)
				}
			}
		})
	}
	wg.Wait()
	if b := bad.Load(); b > 0 {
		log.Printf("%d of %d records failed one by one", b, n)
		return false
	}
	return true
}

// inFlight counts ZeroKMS requests in flight, and the most at once.
type inFlight struct {
	now, max, total atomic.Int64
}

func (f *inFlight) reset() {
	f.now.Store(0)
	f.max.Store(0)
	f.total.Store(0)
}

type inFlightRT struct {
	next http.RoundTripper
	f    *inFlight
}

func (r *inFlightRT) RoundTrip(req *http.Request) (*http.Response, error) {
	n := r.f.now.Add(1)
	r.f.total.Add(1)
	for {
		m := r.f.max.Load()
		if n <= m || r.f.max.CompareAndSwap(m, n) {
			break
		}
	}
	defer r.f.now.Add(-1)
	return r.next.RoundTrip(req)
}

// errLog logs the error or status of every failed request, and nothing of
// its content.
type errLog struct{ next http.RoundTripper }

func (e errLog) RoundTrip(r *http.Request) (*http.Response, error) {
	t := time.Now()
	resp, err := e.next.RoundTrip(r)
	if err != nil {
		log.Printf("HTTP %s %s after %v: error %v", r.Method, r.URL.Path, time.Since(t), err)
	} else if resp.StatusCode >= 300 {
		log.Printf("HTTP %s %s after %v: status %d", r.Method, r.URL.Path, time.Since(t), resp.StatusCode)
	}
	return resp, err
}
