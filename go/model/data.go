package model

import (
	"encoding/json"
	"fmt"
	"math/rand/v2"
	"strings"
	"time"

	"github.com/cipherstash/stack/languages/golang/encrypt"
)

var (
	firstNames = []string{"Ada", "Grace", "Edsger", "Barbara", "Tony", "Margaret", "Donald", "Frances", "Ken", "Radia", "Alan", "Hedy", "Linus", "Shafi", "Niklaus", "Leslie"}
	LastNames  = []string{"Lovelace", "Hopper", "Dijkstra", "Liskov", "Hoare", "Hamilton", "Knuth", "Allen", "Thompson", "Perlman", "Turing", "Lamarr", "Torvalds", "Goldwasser", "Wirth", "Lamport"}
	countries  = []string{"Australia", "New Zealand", "United Kingdom", "United States", "Canada", "Singapore", "Germany", "France"}
	cities     = []string{"Sydney", "Melbourne", "Auckland", "London", "New York", "Toronto", "Singapore", "Berlin", "Paris"}
	jobs       = []string{"Engineer", "Accountant", "Teacher", "Nurse", "Lawyer", "Architect", "Chef", "Pilot"}
	words      = strings.Fields("lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor incididunt ut labore et dolore magna aliqua enim ad minim veniam quis nostrud exercitation ullamco laboris nisi aliquip ex ea commodo consequat")
)

func pick(r *rand.Rand, xs []string) string { return xs[r.IntN(len(xs))] }

// NewRand is the seeded generator the benches draw from, so every run
// encrypts the same plaintexts.
func NewRand(seed uint64) *rand.Rand { return rand.New(rand.NewPCG(seed, seed^0x9e3779b97f4a7c15)) }

// GenIndividual is one synthetic person.
func GenIndividual(r *rand.Rand, id int64) Individual {
	fn, ln := pick(r, firstNames), pick(r, LastNames)
	dob := time.Date(1940+r.IntN(65), time.Month(1+r.IntN(12)), 1+r.IntN(28), 0, 0, 0, 0, time.UTC)
	return Individual{
		ID:           id,
		FirstName:    fn,
		LastName:     ln,
		Email:        EmailFor(id),
		Phone:        fmt.Sprintf("+61 4%02d %03d %03d", r.IntN(100), r.IntN(1000), r.IntN(1000)),
		DateOfBirth:  encrypt.DateOf(dob),
		Nationality:  pick(r, countries),
		TaxID:        fmt.Sprintf("%03d-%03d-%03d", r.IntN(1000), r.IntN(1000), r.IntN(1000)),
		AddressLine1: fmt.Sprintf("%d %s Street", 1+r.IntN(400), pick(r, LastNames)),
		AddressLine2: fmt.Sprintf("Unit %d", 1+r.IntN(60)),
		City:         pick(r, cities),
		Postcode:     fmt.Sprintf("%04d", r.IntN(10000)),
		Country:      pick(r, countries),
		Occupation:   pick(r, jobs),
		RiskScore:    int32(r.IntN(1000)),
	}
}

// EmailFor is row id's email: unique, and derivable from the id alone so
// an equality probe needs no lookup. The Rust ingest writes the same.
func EmailFor(id int64) string { return fmt.Sprintf("person%d@example.com", id) }

// GenText is about n bytes of words.
func GenText(r *rand.Rand, n int) string {
	var b strings.Builder
	for b.Len() < n {
		if b.Len() > 0 {
			b.WriteByte(' ')
		}
		b.WriteString(words[r.IntN(len(words))])
	}
	return b.String()[:n]
}

// GenJSON is a JSON document of about n bytes: a profile with nested
// objects and an array of notes.
func GenJSON(r *rand.Rand, id int64, n int) json.RawMessage {
	doc := map[string]any{
		"id":      id,
		"name":    pick(r, firstNames) + " " + pick(r, LastNames),
		"address": map[string]any{"city": pick(r, cities), "country": pick(r, countries), "postcode": fmt.Sprintf("%04d", r.IntN(10000))},
		"tags":    []string{pick(r, jobs), pick(r, jobs)},
		"score":   r.IntN(1000),
		"active":  r.IntN(2) == 1,
	}
	var notes []string
	for {
		b, _ := json.Marshal(doc)
		if len(b) >= n-80 {
			return b
		}
		notes = append(notes, GenText(r, 64))
		doc["notes"] = notes
	}
}
