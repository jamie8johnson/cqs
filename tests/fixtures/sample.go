// Package sample provides sample functions for testing
package sample

import "fmt"

// Greet returns a greeting message
func Greet(name string) string {
	return fmt.Sprintf("Hello, %s!", name)
}

// Max returns the maximum of two integers
func Max(a, b int) int {
	if a > b {
		return a
	}
	return b
}

// Stack represents a simple stack data structure
type Stack struct {
	items []interface{}
}

// Push adds an item to the stack
func (s *Stack) Push(item interface{}) {
	s.items = append(s.items, item)
}

// Pop removes and returns the top item
func (s *Stack) Pop() interface{} {
	if len(s.items) == 0 {
		return nil
	}
	item := s.items[len(s.items)-1]
	s.items = s.items[:len(s.items)-1]
	return item
}

// Len returns the number of items in the stack
func (s *Stack) Len() int {
	return len(s.items)
}
