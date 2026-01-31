/**
 * Sample TypeScript module for testing
 */

/**
 * Formats a name with a title
 */
export function formatName(first: string, last: string): string {
    return `${first} ${last}`;
}

/**
 * Calculates the average of numbers
 */
export function average(numbers: number[]): number {
    if (numbers.length === 0) return 0;
    return numbers.reduce((a, b) => a + b, 0) / numbers.length;
}

/**
 * Arrow function to double a number
 */
export const double = (x: number): number => x * 2;

/**
 * A simple user class
 */
export class User {
    constructor(public name: string, public age: number) {}

    /**
     * Gets a greeting for this user
     */
    greet(): string {
        return `Hello, I'm ${this.name}`;
    }

    /**
     * Checks if user is adult
     */
    isAdult(): boolean {
        return this.age >= 18;
    }
}
