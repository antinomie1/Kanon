/**
 * Registers the Material 3 Expressive custom elements the console uses.
 *
 * Google's implementation lives in two places: `@material/web/labs/gb/components/*` carries the
 * Material 3 Expressive elements (`md-gb-*` — button, icon button, fab, split button, switch,
 * checkbox, radio, card, list, menu, badge, divider), and the stable `@material/web/*` tree
 * carries the ones the expressive set does not have yet (text field, select, dialog, tabs, chips,
 * progress, slider, navigation, segmented button).
 *
 * Elements are imported one by one rather than through the package's `all.js`, so the bundle only
 * carries what the console renders. Import an element here in the same change that starts using
 * it, never ahead of time.
 */

// Buttons and selection controls (Material 3 Expressive set).
import '@material/web/labs/gb/components/button/md-gb-button.js';
import '@material/web/labs/gb/components/checkbox/md-gb-checkbox.js';
import '@material/web/labs/gb/components/switch/md-gb-switch.js';

// Segmented button (stable set: the expressive tree has no segmented control yet).
import '@material/web/labs/segmentedbutton/outlined-segmented-button.js';
import '@material/web/labs/segmentedbuttonset/outlined-segmented-button-set.js';

// Text fields (stable set: the expressive tree has no field yet).
import '@material/web/textfield/filled-text-field.js';
