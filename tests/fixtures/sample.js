/**
 * Sample JavaScript module for testing
 */

/**
 * Validates an email address
 * @param {string} email - The email to validate
 * @returns {boolean} - Whether the email is valid
 */
function validateEmail(email) {
    const pattern = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;
    return pattern.test(email);
}

/**
 * Generates a random ID
 * @returns {string} - A random ID
 */
function generateId() {
    return Math.random().toString(36).substring(2, 15);
}

/**
 * Arrow function to capitalize a string
 */
const capitalize = (str) => {
    if (!str) return '';
    return str.charAt(0).toUpperCase() + str.slice(1);
};

/**
 * Debounces a function
 */
const debounce = (fn, delay) => {
    let timer = null;
    return (...args) => {
        clearTimeout(timer);
        timer = setTimeout(() => fn(...args), delay);
    };
};

module.exports = { validateEmail, generateId, capitalize, debounce };
