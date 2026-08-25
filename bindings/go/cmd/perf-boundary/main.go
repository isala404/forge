package main

import (
	"encoding/json"
	"flag"
	"fmt"
	"os"
	"sort"
	"time"

	forge "github.com/isala404/forge/bindings/go"
)

type metric struct {
	Name  string  `json:"name"`
	Value float64 `json:"value"`
	Unit  string  `json:"unit"`
}

type report struct {
	SchemaVersion int      `json:"schema_version"`
	Kind          string   `json:"kind"`
	Language      string   `json:"language"`
	Iterations    int      `json:"iterations"`
	Metrics       []metric `json:"metrics"`
}

func main() {
	iterations := flag.Int("iterations", 1000, "number of measured round trips")
	output := flag.String("output", "", "optional JSON report path")
	flag.Parse()
	if *iterations < 1 {
		fmt.Fprintln(os.Stderr, "iterations must be positive")
		os.Exit(2)
	}
	event := forge.CloudEvent{ID: "benchmark", Source: "urn:forge:performance", Type: "forge.benchmark", Data: []byte("boundary")}
	samples := make([]time.Duration, 0, *iterations)
	for range *iterations {
		started := time.Now()
		encoded, err := forge.EncodeCloudEvent(event)
		if err != nil {
			fail(err)
		}
		if _, err := forge.DecodeCloudEvent(encoded); err != nil {
			fail(err)
		}
		samples = append(samples, time.Since(started))
	}
	sort.Slice(samples, func(i, j int) bool { return samples[i] < samples[j] })
	rank := ((*iterations*95)+99)/100 - 1
	result := report{SchemaVersion: 1, Kind: "language_boundary", Language: "go", Iterations: *iterations, Metrics: []metric{{Name: "cloudevent_roundtrip_p95_ms", Value: float64(samples[rank]) / float64(time.Millisecond), Unit: "ms"}}}
	encoded, err := json.MarshalIndent(result, "", "  ")
	if err != nil {
		fail(err)
	}
	encoded = append(encoded, '\n')
	if *output == "" {
		_, err = os.Stdout.Write(encoded)
	} else {
		err = os.WriteFile(*output, encoded, 0o644)
	}
	if err != nil {
		fail(err)
	}
}

func fail(err error) {
	fmt.Fprintln(os.Stderr, err)
	os.Exit(1)
}
