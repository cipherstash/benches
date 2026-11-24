// Command roundtrip checks that values one build of the Go SDK encrypts
// decrypt under another, against real ZeroKMS (verification of
// cipherstash/stack#1773).
//
//	roundtrip -mode encrypt -out f.json [-seed N] [-sizes 1,499,...]
//	roundtrip -mode decrypt -in f.json
//
// Plaintexts are generated from the seed, so the file holds only the seed
// and the ciphertexts. Each size is run twice: as that many one-column
// ShortText values, and as ceil(size/14) 14-column Individuals, whose batch
// mixes text, match, ORE and date columns. A decrypt checks every value,
// once as one batch and once record by record.
package main

import (
	"context"
	"encoding/json"
	"flag"
	"fmt"
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
	Build string     `json:"build"`
	Seed  uint64     `json:"seed"`
	Cases []fileCase `json:"cases"`
}

type fileCase struct {
	Kind        string                      `json:"kind"`
	Size        int                         `json:"size"`
	ShortTexts  []model.EncryptedShortText  `json:"short_texts,omitempty"`
	Individuals []model.EncryptedIndividual `json:"individuals,omitempty"`
}

func shortTexts(seed uint64, size int) []model.ShortText {
	r := model.NewRand(seed ^ uint64(size)*0x100000001b3)
	out := make([]model.ShortText, size)
	for k := range out {
		ind := model.GenIndividual(r, int64(k))
		out[k] = model.ShortText{ID: int64(k), FirstName: fmt.Sprintf("%s-%d", ind.FirstName, k)}
	}
	return out
}

func individuals(seed uint64, size int) []model.Individual {
	r := model.NewRand(seed ^ uint64(size)*0xcbf29ce484222325)
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

func main() {
	mode := flag.String("mode", "", "encrypt | decrypt")
	out := flag.String("out", "", "encrypt: write here")
	in := flag.String("in", "", "decrypt: read this")
	seed := flag.Uint64("seed", uint64(time.Now().UnixNano()), "plaintext seed")
	sizesFlag := flag.String("sizes", "1,499,500,501,1499,1500,1501,2501,10000", "batch sizes in values")
	workers := flag.Int("workers", 16, "decrypt: concurrent record-by-record decrypts")
	onlyKind := flag.String("only-kind", "", "decrypt: only this kind")
	onlySize := flag.Int("only-size", 0, "decrypt: only this size")
	logHTTP := flag.Bool("log-http", false, "log every failed HTTP request")
	flag.Parse()

	ctx := context.Background()
	var opts []encrypt.ClientOption
	if *logHTTP {
		opts = append(opts, encrypt.WithTransport(errLog{http.DefaultTransport}))
	}
	client, err := encrypt.NewClient(ctx, opts...)
	if err != nil {
		log.Fatalf("NewClient: %v", err)
	}
	defer client.Close()
	cipher := client.DefaultKeyset()
	log.Printf("build %s", build())

	switch *mode {
	case "encrypt":
		f := file{Build: build(), Seed: *seed}
		for _, s := range strings.Split(*sizesFlag, ",") {
			size, err := strconv.Atoi(s)
			if err != nil {
				log.Fatal(err)
			}
			st, err := model.EncryptShortTexts(ctx, cipher, shortTexts(*seed, size))
			if err != nil {
				log.Fatalf("encrypt %d short texts: %v", size, err)
			}
			f.Cases = append(f.Cases, fileCase{Kind: "short_text", Size: size, ShortTexts: st})
			ind, err := model.EncryptIndividuals(ctx, cipher, individuals(*seed, size))
			if err != nil {
				log.Fatalf("encrypt %d individuals: %v", size, err)
			}
			f.Cases = append(f.Cases, fileCase{Kind: "individual", Size: size, Individuals: ind})
			log.Printf("encrypted size %d: %d short texts, %d individuals (%d values)", size, len(st), len(ind), 14*len(ind))
		}
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
		for _, c := range f.Cases {
			if (*onlyKind != "" && c.Kind != *onlyKind) || (*onlySize != 0 && c.Size != *onlySize) {
				continue
			}
			batchOK, singleOK, values := check(ctx, cipher, f.Seed, c, *workers)
			status := "PASS"
			if !batchOK || !singleOK {
				status, failed = "FAIL", true
			}
			// One line per case, parsed into the report.
			fmt.Printf("RESULT\tenc=%s\tdec=%s\tkind=%s\tsize=%d\tvalues=%d\tbatch=%v\tsingle=%v\t%s\n",
				f.Build, build(), c.Kind, c.Size, values, batchOK, singleOK, status)
		}
		if failed {
			os.Exit(1)
		}
	default:
		log.Fatalf("-mode encrypt|decrypt")
	}
}

// check decrypts a case as one batch and record by record, and compares
// every record with its plaintext.
func check(ctx context.Context, cipher *encrypt.Cipher, seed uint64, c fileCase, workers int) (batch, single bool, values int) {
	switch c.Kind {
	case "short_text":
		want := shortTexts(seed, c.Size)
		values = len(want)
		if len(c.ShortTexts) != len(want) {
			log.Printf("%s %d: %d ciphertexts, want %d", c.Kind, c.Size, len(c.ShortTexts), len(want))
			return false, false, values
		}
		got, err := model.DecryptShortTexts(ctx, cipher, c.ShortTexts)
		batch = err == nil && len(got) == len(want)
		if err != nil {
			log.Printf("%s %d batch: %v", c.Kind, c.Size, err)
		}
		for k := range got {
			if batch && got[k] != want[k] {
				log.Printf("%s %d batch: record %d differs", c.Kind, c.Size, k)
				batch = false
			}
		}
		single = each(len(want), workers, func(k int) bool {
			g, err := model.DecryptShortTexts(ctx, cipher, c.ShortTexts[k:k+1])
			if err != nil {
				log.Printf("%s %d record %d: %v", c.Kind, c.Size, k, err)
				return false
			}
			return len(g) == 1 && g[0] == want[k]
		})
	case "individual":
		want := individuals(seed, c.Size)
		values = 14 * len(want)
		if len(c.Individuals) != len(want) {
			log.Printf("%s %d: %d ciphertexts, want %d", c.Kind, c.Size, len(c.Individuals), len(want))
			return false, false, values
		}
		got, err := model.DecryptIndividuals(ctx, cipher, c.Individuals)
		batch = err == nil && len(got) == len(want)
		if err != nil {
			log.Printf("%s %d batch: %v", c.Kind, c.Size, err)
		}
		for k := range got {
			if batch && got[k] != want[k] {
				log.Printf("%s %d batch: record %d differs", c.Kind, c.Size, k)
				batch = false
			}
		}
		single = each(len(want), workers, func(k int) bool {
			g, err := model.DecryptIndividuals(ctx, cipher, c.Individuals[k:k+1])
			if err != nil {
				log.Printf("%s %d record %d: %v", c.Kind, c.Size, k, err)
				return false
			}
			return len(g) == 1 && g[0] == want[k]
		})
	}
	return batch, single, values
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
