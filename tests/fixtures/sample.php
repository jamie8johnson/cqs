<?php

namespace App\Models;

/**
 * Represents a user in the system.
 */
class User
{
    private string $name;
    private int $age;

    public function __construct(string $name, int $age)
    {
        $this->name = $name;
        $this->age = $age;
    }

    public function getName(): string
    {
        return $this->name;
    }

    public function greet(): string
    {
        return "Hello, " . $this->name;
    }
}

interface Printable
{
    public function print(): void;
}

trait Timestampable
{
    public function getCreatedAt(): string
    {
        return date('Y-m-d');
    }
}

enum Status: string
{
    case Active = 'active';
    case Inactive = 'inactive';
    case Suspended = 'suspended';
}

function formatDuration(int $seconds): string
{
    $hours = intdiv($seconds, 3600);
    $minutes = intdiv($seconds % 3600, 60);
    return "{$hours}h {$minutes}m";
}
