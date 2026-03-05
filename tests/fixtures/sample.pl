#!/usr/bin/perl
use strict;
use warnings;

package Calculator;

# Create a new calculator
sub new {
    my ($class) = @_;
    return bless { history => [] }, $class;
}

# Add two numbers
sub add {
    my ($self, $a, $b) = @_;
    my $result = $a + $b;
    push @{$self->{history}}, $result;
    return $result;
}

# Multiply two numbers
sub multiply {
    my ($self, $a, $b) = @_;
    return $a * $b;
}

# Get the last result
sub last_result {
    my ($self) = @_;
    return $self->{history}[-1];
}

package main;

sub greet {
    my ($name) = @_;
    print "Hello, $name!\n";
    return 1;
}

sub process_data {
    my ($data) = @_;
    my $result = transform($data);
    validate($result);
    return $result;
}

sub transform {
    my ($data) = @_;
    return uc($data);
}

sub validate {
    my ($result) = @_;
    die "Invalid" unless defined $result;
    return 1;
}

1;
