// Command wasmrun runs a WASI command module under wazero, configured as
// the Go SDK configures its guest: the optimizing compiler, a real clock,
// and WithCloseOnContextDone on or off. For experiments/aead-wasm.
//
//	wasmrun -wasm aead-wasm.wasm [-close] -- <args for the module>
package main

import (
	"context"
	"flag"
	"log"
	"os"

	"github.com/tetratelabs/wazero"
	"github.com/tetratelabs/wazero/imports/wasi_snapshot_preview1"
	"github.com/tetratelabs/wazero/sys"
)

func main() {
	path := flag.String("wasm", "", "WASI command module")
	closeOnDone := flag.Bool("close", false, "build the runtime WithCloseOnContextDone(true), as rc.6 does")
	flag.Parse()
	wasm, err := os.ReadFile(*path)
	if err != nil {
		log.Fatal(err)
	}
	ctx := context.Background()
	rt := wazero.NewRuntimeWithConfig(ctx, wazero.NewRuntimeConfig().WithCloseOnContextDone(*closeOnDone))
	defer rt.Close(ctx)
	wasi_snapshot_preview1.MustInstantiate(ctx, rt)
	cfg := wazero.NewModuleConfig().
		WithStdout(os.Stdout).WithStderr(os.Stderr).
		WithSysNanotime().WithSysWalltime().
		WithArgs(append([]string{"aead-wasm"}, flag.Args()...)...)
	if _, err := rt.InstantiateWithConfig(ctx, wasm, cfg); err != nil {
		if exit, ok := err.(*sys.ExitError); !ok || exit.ExitCode() != 0 {
			log.Fatal(err)
		}
	}
}
