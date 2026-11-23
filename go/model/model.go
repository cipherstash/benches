// Package model holds the records the Go benches encrypt and decrypt.
//
// The shapes mirror the case a customer reported against Go SDK rc.6:
// a 14-column "individual" record decrypted 100 rows at a time, and the
// single-column cases they used to isolate the cost (one short text, one
// 2 KB text, one 2 KB JSON document).
//
// The Rust ingest in ../../v4 writes the same columns under the same
// contexts (scope "individuals" / "documents", then the column name), so
// the Go benches can read rows the Rust side stored.
package model

import (
	"encoding/json"

	"github.com/cipherstash/stack/languages/golang/encrypt"
)

//go:generate go tool stashgen -type Individual -name Individuals -redact
//go:generate go tool stashgen -type ShortText -name ShortTexts -redact
//go:generate go tool stashgen -type LongText -name LongTexts -redact
//go:generate go tool stashgen -type JSONDoc -name JSONDocs -redact

// Individual is a 14-column person record: every column encrypted.
type Individual struct {
	_            struct{}     `stash:"context=individuals"`
	ID           int64        `stash:"id,passthrough"`
	FirstName    string       `stash:"first_name,encrypt_into=TextEq"`
	LastName     string       `stash:"last_name,encrypt_into=TextMatch"`
	Email        string       `stash:"email,encrypt_into=TextEq"`
	Phone        string       `stash:"phone,encrypt_into=Text"`
	DateOfBirth  encrypt.Date `stash:"date_of_birth,encrypt_into=Date"`
	Nationality  string       `stash:"nationality,encrypt_into=Text"`
	TaxID        string       `stash:"tax_id,encrypt_into=TextEq"`
	AddressLine1 string       `stash:"address_line1,encrypt_into=Text"`
	AddressLine2 string       `stash:"address_line2,encrypt_into=Text"`
	City         string       `stash:"city,encrypt_into=Text"`
	Postcode     string       `stash:"postcode,encrypt_into=Text"`
	Country      string       `stash:"country,encrypt_into=Text"`
	Occupation   string       `stash:"occupation,encrypt_into=Text"`
	RiskScore    int32        `stash:"risk_score,encrypt_into=IntegerOrd"`
}

// ShortText is one short text column: the first column of Individual,
// read on its own.
type ShortText struct {
	_         struct{} `stash:"context=individuals"`
	ID        int64    `stash:"id,passthrough"`
	FirstName string   `stash:"first_name,encrypt_into=TextEq"`
}

// LongText is one ~2 KB text column.
type LongText struct {
	_   struct{} `stash:"context=documents"`
	ID  int64    `stash:"id,passthrough"`
	Bio string   `stash:"bio,encrypt_into=Text"`
}

// JSONDoc is one ~2 KB JSON document, sealed whole.
type JSONDoc struct {
	_       struct{}        `stash:"context=documents"`
	ID      int64           `stash:"id,passthrough"`
	Profile json.RawMessage `stash:"profile,encrypt_into=Json"`
}
