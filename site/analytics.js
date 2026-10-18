// Google Analytics via analytics-adapters. No bundler here, so it loads from jsdelivr pinned to a version.
import { createAnalytics } from 'https://cdn.jsdelivr.net/npm/analytics-adapters@1.2.0/dist/index.js';
import { google } from 'https://cdn.jsdelivr.net/npm/analytics-adapters@1.2.0/dist/google.js';

// The adapter turns off gtag's automatic page_view, so each page load sends its own.
createAnalytics(google({ id: 'G-73EV3BQ3CH' })).page();
