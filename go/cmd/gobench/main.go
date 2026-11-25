// Command gobench measures the Go SDK's encrypt and decrypt cost against
// ZeroKMS.
//
//	gobench -mode micro               # the five cases BNDRY reported, no database
//	gobench -mode db -tier 1000000    # pages read from the EQL v4 tables, then decrypted
//
// Every case reports wall time and process CPU time (getrusage user+sys,
// which counts every goroutine: the guest, the GC, the HTTP client) per
// call. CPU is the figure that exposes guest cost: wall time includes the
// ZeroKMS round trip, CPU barely does.
//
// -variant only labels the output. Which guest and host actually run is
// decided by the module graph (see ../../scripts/go-variant.sh).
//
// The same micro cases run offline, under encrypttest's deterministic
// keys, with `go test ./offline` (no credentials needed).
package main

import (
	"context"
	"flag"
	"log"
	"time"

	"github.com/cipherstash/benches/go/harness"
	"github.com/cipherstash/stack/languages/golang/encrypt"
	_ "github.com/cipherstash/stack/languages/golang/encrypt/eql"
)

func main() {
	mode := flag.String("mode", "micro", "micro | db")
	flag.StringVar(&harness.Variant, "variant", "shipped", "label for the guest/host build under test")
	rounds := flag.Int("rounds", 2, "rounds per case")
	calls := flag.Int("calls", 3, "calls per round")
	tier := flag.Int("tier", 10000, "db mode: table tier")
	clients := flag.Int("clients", 1, "db mode: split each page across this many Clients")
	outPath := flag.String("out", "", "append JSON lines here")
	only := flag.String("only", "", "run only the scenario with this name")
	flag.Parse()

	if *outPath != "" {
		closeOut, err := harness.SetOut(*outPath)
		if err != nil {
			log.Fatal(err)
		}
		defer closeOut()
	}

	ctx := context.Background()
	t0 := time.Now()
	client, err := encrypt.NewClient(ctx)
	if err != nil {
		log.Fatalf("NewClient: %v", err)
	}
	defer client.Close()
	cipher := client.DefaultKeyset()
	log.Printf("client ready in %v", time.Since(t0))

	switch *mode {
	case "micro":
		harness.RunMicro(ctx, cipher, "micro", *rounds, *calls, *only)
	case "db":
		harness.RunDB(ctx, cipher, *tier, *rounds, *calls, *clients, *only)
	default:
		log.Fatalf("unknown -mode %q", *mode)
	}
}
