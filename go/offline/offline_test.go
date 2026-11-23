// Package offline runs the micro cases against encrypttest's deterministic
// key source: the same guest and host as production, no ZeroKMS. It
// isolates the host/guest CPU cost and needs no credentials. It does not
// run recipher's key derivation, which a ZeroKMS decrypt does per value,
// so absolute figures are lower than the live ones.
//
//	go test ./offline -run TestMicro -v -args -variant shipped -out ../results.jsonl
package offline

import (
	"context"
	"flag"
	"testing"

	"github.com/cipherstash/benches/go/harness"
	"github.com/cipherstash/stack/languages/golang/encrypt/encrypttest"
)

var (
	variant = flag.String("variant", "shipped", "label for the build under test")
	out     = flag.String("out", "", "append JSON lines here")
	rounds  = flag.Int("rounds", 2, "rounds per case")
	calls   = flag.Int("calls", 3, "calls per round")
	only    = flag.String("only", "", "run only this scenario")
)

func TestMicro(t *testing.T) {
	harness.Variant = *variant
	if *out != "" {
		closeOut, err := harness.SetOut(*out)
		if err != nil {
			t.Fatal(err)
		}
		defer closeOut()
	}
	client := encrypttest.NewClient(t)
	harness.RunMicro(context.Background(), client.DefaultKeyset(), "offline", *rounds, *calls, *only)
}
