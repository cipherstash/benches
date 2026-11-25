package harness

import (
	"context"
	"fmt"
	"log"
	"math/rand/v2"
	"os"
	"strings"
	"sync"
	"time"

	"github.com/cipherstash/benches/go/model"
	"github.com/cipherstash/stack/languages/golang/encrypt"
	"github.com/cipherstash/stack/languages/golang/encrypt/eql"
	"github.com/jackc/pgx/v5"
)

// columns is the select list, in EncryptedIndividual's field order. Each
// EQL column is read as its JSON text, which is what an eql type holds.
var columns = []string{"first_name", "last_name", "email", "phone", "date_of_birth", "nationality", "tax_id",
	"address_line1", "address_line2", "city", "postcode", "country", "occupation", "risk_score"}

func selectList() string {
	parts := []string{"id"}
	for _, c := range columns {
		parts = append(parts, c+"::jsonb")
	}
	return strings.Join(parts, ", ")
}

func scanIndividuals(rows pgx.Rows) ([]model.EncryptedIndividual, error) {
	var out []model.EncryptedIndividual
	for rows.Next() {
		var e model.EncryptedIndividual
		var b [14][]byte
		dst := []any{&e.ID}
		for i := range b {
			dst = append(dst, &b[i])
		}
		if err := rows.Scan(dst...); err != nil {
			return nil, err
		}
		e.FirstName, e.LastName, e.Email = eql.TextEq(b[0]), eql.TextMatch(b[1]), eql.TextEq(b[2])
		e.Phone, e.DateOfBirth, e.Nationality = eql.Text(b[3]), eql.Date(b[4]), eql.Text(b[5])
		e.TaxID, e.AddressLine1, e.AddressLine2 = eql.TextEq(b[6]), eql.Text(b[7]), eql.Text(b[8])
		e.City, e.Postcode, e.Country = eql.Text(b[9]), eql.Text(b[10]), eql.Text(b[11])
		e.Occupation, e.RiskScore = eql.Text(b[12]), eql.IntegerOrd(b[13])
		out = append(out, e)
	}
	return out, rows.Err()
}

// decryptSplit opens a page across len(ciphers) Clients at once: the
// pool BNDRY tried, since one Client runs one guest call at a time.
func decryptSplit(ctx context.Context, ciphers []*encrypt.Cipher, page []model.EncryptedIndividual) error {
	if len(ciphers) == 1 {
		_, err := model.DecryptIndividuals(ctx, ciphers[0], page)
		return err
	}
	n := len(ciphers)
	chunk := (len(page) + n - 1) / n
	var wg sync.WaitGroup
	errs := make([]error, n)
	for i := 0; i < n; i++ {
		lo, hi := i*chunk, min((i+1)*chunk, len(page))
		if lo >= hi {
			continue
		}
		wg.Add(1)
		go func() {
			defer wg.Done()
			_, errs[i] = model.DecryptIndividuals(ctx, ciphers[i], page[lo:hi])
		}()
	}
	wg.Wait()
	for _, err := range errs {
		if err != nil {
			return err
		}
	}
	return nil
}

func RunDB(ctx context.Context, cipher *encrypt.Cipher, tier, rounds, calls, clients int, only string) {
	url := os.Getenv("DATABASE_URL")
	if url == "" {
		url = "postgres://postgres:postgres@localhost:5400/postgres"
	}
	conn, err := pgx.Connect(ctx, url)
	if err != nil {
		log.Fatal(err)
	}
	defer conn.Close(ctx)
	table := fmt.Sprintf("individuals_v4_%d", tier)

	ciphers := []*encrypt.Cipher{cipher}
	for i := 1; i < clients; i++ {
		c, err := encrypt.NewClient(ctx)
		if err != nil {
			log.Fatal(err)
		}
		defer c.Close()
		ciphers = append(ciphers, c.DefaultKeyset())
	}

	r := rand.New(rand.NewPCG(7, 11))

	type kase struct {
		name  string
		query func() (pgx.Rows, error)
	}
	cases := []kase{
		{"page_100_by_id", func() (pgx.Rows, error) {
			start := r.Int64N(int64(max(tier-100, 1)))
			return conn.Query(ctx, "SELECT "+selectList()+" FROM "+table+" WHERE id > $1 ORDER BY id LIMIT 100", start)
		}},
		{"eq_email_1", func() (pgx.Rows, error) {
			// Both ingests write person<id>@example.com, so any id is one row.
			id := 1 + r.Int64N(int64(tier))
			q, err := model.IndividualsFields.Email.Eq(ctx, cipher, model.EmailFor(id))
			if err != nil {
				return nil, err
			}
			return conn.Query(ctx, "SELECT "+selectList()+" FROM "+table+" WHERE email = $1::eql_v4.query_text_eq", q)
		}},
		{"match_last_name_100", func() (pgx.Rows, error) {
			q, err := model.IndividualsFields.LastName.Matches(ctx, cipher, model.LastNames[r.IntN(len(model.LastNames))])
			if err != nil {
				return nil, err
			}
			return conn.Query(ctx, "SELECT "+selectList()+" FROM "+table+" WHERE last_name @@ $1::eql_v4.query_text_match LIMIT 100", q.Query())
		}},
		{"ord_risk_gt_100", func() (pgx.Rows, error) {
			q, err := model.IndividualsFields.RiskScore.Gt(ctx, cipher, int32(r.IntN(990)))
			if err != nil {
				return nil, err
			}
			return conn.Query(ctx, "SELECT "+selectList()+" FROM "+table+
				" WHERE risk_score > $1::eql_v4.query_integer_ord ORDER BY eql_v4.ord_term(risk_score) LIMIT 100", q)
		}},
	}

	for _, k := range cases {
		if only != "" && k.name != only {
			continue
		}
		fetch := func() ([]model.EncryptedIndividual, time.Duration, error) {
			t0 := time.Now()
			rows, err := k.query()
			if err != nil {
				return nil, 0, err
			}
			page, err := scanIndividuals(rows)
			return page, time.Since(t0), err
		}
		// warm up
		page, _, err := fetch()
		if err != nil {
			log.Fatalf("%s: %v", k.name, err)
		}
		if err := decryptSplit(ctx, ciphers, page); err != nil {
			log.Fatalf("%s decrypt: %v", k.name, err)
		}
		var wall, cpu, qms []float64
		rowsSeen := 0
		for range rounds * calls {
			page, qd, err := fetch()
			if err != nil {
				log.Fatalf("%s: %v", k.name, err)
			}
			rowsSeen = len(page)
			w, c, err := measure(1, func() error { return decryptSplit(ctx, ciphers, page) })
			if err != nil {
				log.Fatalf("%s decrypt: %v", k.name, err)
			}
			wall, cpu, qms = append(wall, w...), append(cpu, c...), append(qms, ms(qd))
		}
		emit(Result{Mode: "db", Tier: tier, Scenario: k.name, Op: "decrypt", Rows: rowsSeen, Values: rowsSeen * 14,
			Clients: clients, WallMS: wall, CPUMS: cpu, QueryMS: qms})
	}
}
