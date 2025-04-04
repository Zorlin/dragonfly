/**
 * Dragonfly Gamepad Support
 * Provides Xbox, PlayStation, and other gamepad controller support
 * with a No Man's Sky-inspired cursor that snaps to UI elements
 */

class GamepadController {
    constructor() {
        // Add an initialization flag
        if (window.gamepadControllerInitialized) {
            console.log('[Gamepad] Controller already initialized, aborting duplicate initialization');
            return; // Exit constructor if already initialized
        }
        window.gamepadControllerInitialized = true;
        
        // Controller state
        this.gamepads = [];
        this.gamepadConnected = false;
        this.activeElement = null;
        this.focusableElements = [];
        this.topNavTabs = [];
        this.currentElementIndex = 0;
        this.buttonStates = {};
        this.lastButtonState = {}; // Add tracking of physical button state
        this.waitingForReleaseCount = 0; // Add counter for "waiting for release" occurrences
        this.analogMoved = false;
        this.gamepadPollingInterval = null;
        this.lastModalState = false;
        this.freeCursorElement = null;
        this.freeCursorVisible = false;
        this.freeCursorHideTimer = null;
        this.freeCursorPosition = { x: window.innerWidth / 2, y: window.innerHeight / 2 }; // Start in center
        this.DEADZONE = 0.15; // Define the deadzone value
        this.isClicking = false; // ADD flag to debounce clicks
        this.isTogglingTheme = false; // Add flag to debounce theme toggle
        this.aButtonHeldDown = false; // Add this flag, initialized to false
        this.lbButtonHeldDown = false; // Flag for Left Bumper
        
        // Initialize
        this.init();
    }
    
    init() {
        // Setup gamepad event listeners
        window.addEventListener('gamepadconnected', this.handleGamepadConnected.bind(this));
        window.addEventListener('gamepaddisconnected', this.handleGamepadDisconnected.bind(this));
        
        // Add a listener to clear state before page unloads
        window.addEventListener('beforeunload', () => {
            console.log('[Gamepad] beforeunload: Stopping polling and clearing states.');
            this.stopGamepadPolling(); 
            this.buttonStates = {}; // Explicitly clear states
        });
        
        // Check for already connected gamepads
        setTimeout(() => this.checkForExistingGamepads(), 100);
        
        // Create the free-floating cursor element (initially hidden)
        this.freeCursorElement = document.createElement('div');
        this.freeCursorElement.id = 'free-cursor';
        this.freeCursorElement.style.position = 'fixed';
        this.freeCursorElement.style.left = '0px'; // Position updated dynamically
        this.freeCursorElement.style.top = '0px';
        this.freeCursorElement.style.zIndex = '10000';
        this.freeCursorElement.style.display = 'none'; // Initially hidden
        this.freeCursorElement.style.pointerEvents = 'none'; // Don't interfere with mouse clicks
        document.body.appendChild(this.freeCursorElement);
        
        // Initialize focusable elements on page load
        document.addEventListener('DOMContentLoaded', () => {
            this.updateFocusableElements();
            
            // Watch for modal state changes
            this.setupModalObserver();
            
            // Find top navigation tabs once DOM is ready
            this.findTopTabs();
        });
    }
    
    setupModalObserver() {
        // Watch for changes to modals becoming visible/hidden
        setInterval(() => {
            const modalVisible = !!document.querySelector('div[aria-modal="true"]:not([x-cloak])');
            
            // If modal state changed
            if (modalVisible !== this.lastModalState) {
                this.lastModalState = modalVisible;
                
                // Update focusable elements and focus the first one in the modal
                setTimeout(() => {
                    this.updateFocusableElements();
                    if (modalVisible) {
                        const modalElements = this.getElementsInActiveModal();
                        if (modalElements.length > 0) {
                            const firstModalElement = modalElements[0];
                            const index = this.focusableElements.indexOf(firstModalElement);
                            if (index !== -1) {
                                this.focusElementAtIndex(index);
                            }
                        }
                    }
                }, 100);
            }
        }, 200);
    }
    
    checkForExistingGamepads() {
        const gamepads = navigator.getGamepads ? navigator.getGamepads() : [];
        console.log('Checking for existing gamepads:', gamepads);
        let foundOne = false;
        for (let i = 0; i < gamepads.length; i++) {
            if (gamepads[i]) {
                console.log('Found existing gamepad:', gamepads[i]);
                // Manually trigger the connection handler for the existing gamepad
                this.handleGamepadConnected({ gamepad: gamepads[i] });
                foundOne = true;
            }
        }
        // If any existing gamepad was found, dispatch the connected event
        if (foundOne) {
            window.dispatchEvent(new CustomEvent('gamepad-connection-changed', { detail: { connected: true } }));
        }
    }
    
    getElementsInActiveModal() {
        // Find the visible modal
        const visibleModal = document.querySelector('div[aria-modal="true"]:not([x-cloak])');
        if (!visibleModal) return [];
        
        // Get all focusable elements within that modal
        return this.focusableElements.filter(el => visibleModal.contains(el));
    }
    
    handleGamepadConnected(e) {
        console.log('Gamepad connected handler triggered for:', e.gamepad.id);
        
        // Check if this gamepad is already connected
        if (this.gamepads[e.gamepad.index]) {
            console.log('[Gamepad] This gamepad is already registered:', e.gamepad.id);
            return;
        }
        
        console.log('Gamepad connected:', e.gamepad.id);
        this.gamepads[e.gamepad.index] = e.gamepad;
        this.gamepadConnected = true;
        this.showGamepadUI(); // Updates focusable elements and sets initial focus
        this.startGamepadPolling();
        
        // Dispatch event for Alpine.js
        window.dispatchEvent(new CustomEvent('gamepad-connection-changed', { detail: { connected: true } }));
    }
    
    handleGamepadDisconnected(e) {
        console.log('Gamepad disconnected:', e.gamepad.id);
        delete this.gamepads[e.gamepad.index];
        this.gamepadConnected = Object.keys(this.gamepads).length > 0;
        
        if (!this.gamepadConnected) {
            this.hideGamepadUI();
            this.stopGamepadPolling();
            // Dispatch event for Alpine.js ONLY when the *last* gamepad is disconnected
            window.dispatchEvent(new CustomEvent('gamepad-connection-changed', { detail: { connected: false } }));
        }
    }
    
    showGamepadUI() {
        console.log('showGamepadUI called');
        this.updateFocusableElements();
        
        // Add styles if not already present
        if (!document.getElementById('gamepad-styles')) {
            const styleEl = document.createElement('style');
            styleEl.id = 'gamepad-styles';
            styleEl.textContent = `
                .gamepad-focus {
                    outline: 3px solid rgba(99, 102, 241, 0.8) !important;
                    outline-offset: 4px !important;
                    position: relative;
                    z-index: 40;
                    box-shadow: 0 0 15px rgba(99, 102, 241, 0.5);
                    animation: pulse 2s cubic-bezier(0.4, 0, 0.6, 1) infinite;
                }
                
                tr.gamepad-focus {
                    outline-offset: -1px !important;
                }
                
                @keyframes pulse {
                    0%, 100% { opacity: 1; }
                    50% { opacity: 0.7; }
                }
                
                /* Styles for the free-floating NMS-style cursor */
                #free-cursor {
                    pointer-events: none;
                    width: 48px; /* Size of the outer ring */
                    height: 48px;
                    border: 2px solid rgba(255, 255, 255, 0.8); /* Outer ring */
                    border-radius: 50%;
                    transition: transform 0.1s ease-out;
                    display: flex; /* Use flex to center the inner dot */
                    align-items: center;
                    justify-content: center;
                }
                
                #free-cursor::before { /* Inner dot */
                    content: '';
                    display: block;
                    width: 4px;
                    height: 4px;
                    background-color: rgba(255, 255, 255, 0.9);
                    border-radius: 50%;
                }
                
                #gamepad-hint {
                    transition: opacity 0.5s ease;
                }
            `;
            document.head.appendChild(styleEl);
        }
        
        // Focus the first element
        if (this.focusableElements.length > 0) {
            console.log('Attempting initial focus (index 0)...');
            this.focusElementAtIndex(0);
        } else {
            console.warn('No focusable elements found when showing gamepad UI.');
        }
        
        // Show gamepad controls hint
        const hint = document.createElement('div');
        hint.id = 'gamepad-hint';
        hint.className = 'fixed bottom-4 left-4 bg-black bg-opacity-70 text-white p-3 rounded-lg text-sm z-[9999]';
        hint.innerHTML = `
          <div class="flex items-center space-x-2">
            <span>🎮</span>
            <span>Use D-pad/sticks to navigate, A/X to select, B/Circle to back</span>
          </div>
        `;
        document.body.appendChild(hint);
        setTimeout(() => {
          if (hint) hint.classList.add('opacity-50');
        }, 5000);
    }
    
    hideGamepadUI() {
        const hint = document.getElementById('gamepad-hint');
        if (hint) hint.remove();
        
        this.clearFocusStyles();
    }
    
    updateFocusableElements() {
        // Get all focusable elements (links, buttons, form elements)
        this.focusableElements = Array.from(document.querySelectorAll('a, button, select, input, textarea, [tabindex]:not([tabindex="-1"])'))
          .filter(el => {
            // Ensure element is visible and not disabled
            const style = window.getComputedStyle(el);
            return style.display !== 'none' && 
                   style.visibility !== 'hidden' && 
                   !el.hasAttribute('disabled');
          });
        
        // Include machine list rows
        const machineRows = document.querySelectorAll('tr[data-machine-id]');
        if (machineRows.length) {
            machineRows.forEach(row => {
                if (!this.focusableElements.includes(row)) {
                    this.focusableElements.push(row);
                }
            });
        }
        
        // Include the mode cards in the Add Machine modal (using a more robust selector)
        const addMachineModal = document.getElementById('add-machine-modal');
        let addMachineCards = [];
        if (addMachineModal) {
            addMachineCards = Array.from(addMachineModal.querySelectorAll('.grid > div[class*="cursor-pointer"]'));
        }
        // const addMachineCards = document.querySelectorAll('.grid-cols-1.md\\:grid-cols-3 > div'); // Old selector
        if (addMachineCards.length) {
            addMachineCards.forEach(card => {
                if (!this.focusableElements.includes(card)) {
                    this.focusableElements.push(card);
                }
            });
        }
        
        // Special case: cancel buttons at the bottom of modals
        const modalCancelButtons = document.querySelectorAll('div[aria-modal="true"] button');
        if (modalCancelButtons.length) {
            modalCancelButtons.forEach(button => {
                if (!this.focusableElements.includes(button)) {
                    this.focusableElements.push(button);
                }
            });
        }
        
        // Filter out elements that are hidden by Alpine's x-cloak
        this.focusableElements = this.focusableElements.filter(el => {
            return !el.closest('[x-cloak]');
        });

        // Filter out elements specifically marked for exclusion from gamepad navigation
        this.focusableElements = this.focusableElements.filter(el => 
            !el.classList.contains('gamepad-nav-exclude')
        );
    }
    
    focusElementAtIndex(index) {
        // Always update right before focusing to catch dynamically added/shown elements
        // console.log('Updating focusable elements within focusElementAtIndex...'); // Can be noisy
        this.updateFocusableElements();
        
        if (this.focusableElements.length === 0) {
            console.warn('[Gamepad] No focusable elements found. Cannot focus.');
            return;
        }
        console.log(`[Gamepad] Attempting to focus element at index: ${index}`);
        
        if (index < 0) index = 0;
        if (index >= this.focusableElements.length) index = this.focusableElements.length - 1;
        
        this.currentElementIndex = index;
        this.activeElement = this.focusableElements[index];
        console.log('[Gamepad] Focusing element:', this.activeElement);
        
        // Scroll element into view if needed
        this.activeElement.scrollIntoView({ behavior: 'smooth', block: 'center' });
        
        // Clear any existing focus styles first
        this.clearFocusStyles();
        
        // Add focus styles
        this.activeElement.classList.add('gamepad-focus');
    }
    
    startGamepadPolling() {
        if (this.gamepadPollingInterval) {
            console.log('[Gamepad] Polling already running.');
            return;
        }
        
        console.log('[Gamepad] Clearing button states before starting poll.');
        this.buttonStates = {}; 
        this.isClicking = false; // Reset click debounce flag on startup
        
        // Restore the last physical button state from a previous page if it exists in session storage
        try {
            const savedState = sessionStorage.getItem('gamepadLastButtonState');
            if (savedState) {
                this.lastButtonState = JSON.parse(savedState);
                console.log('[Gamepad] Restored last button state:', this.lastButtonState);
            }
        } catch (e) {
            console.error('[Gamepad] Error restoring last button state:', e);
            this.lastButtonState = {};
        }
        
        console.log('[Gamepad] Starting gamepad polling interval.');
        this.gamepadPollingInterval = setInterval(() => {
            // Get all connected gamepads
            const gamepads = navigator.getGamepads();
            
            for (const gamepad of gamepads) {
                if (!gamepad) continue;
                
                // Handle buttons (check for pressed state changes)
                this.handleGamepadInput(gamepad);
            }
        }, 100); // Poll at 10Hz
    }
    
    stopGamepadPolling() {
        if (this.gamepadPollingInterval) {
            console.log('[Gamepad] Stopping gamepad polling interval and clearing states.');
            clearInterval(this.gamepadPollingInterval);
            this.gamepadPollingInterval = null;
            
            // Save the last physical button state to session storage before page transition
            try {
                sessionStorage.setItem('gamepadLastButtonState', JSON.stringify(this.lastButtonState));
            } catch (e) {
                console.error('[Gamepad] Error saving last button state:', e);
            }
            
            this.buttonStates = {}; // Clear states when polling stops too
            this.isClicking = false; // Clear debounce flag
        }
    }
    
    handleGamepadInput(gamepad) {
        const DEADZONE = 0.15; // Axis deadzone threshold
        
        // Ensure buttonStates object exists
        if (!this.buttonStates) this.buttonStates = {}; 
        if (!this.lastButtonState) this.lastButtonState = {};

        // --- 1. Track physical button states first ---
        // Store the current physical state of all buttons we care about
        const buttonIndices = [0, 1, 4, 6, 7, 8, 9, 12, 13, 14, 15]; // Same buttons as in buttonMapping
        buttonIndices.forEach(index => {
            // If button exists on this gamepad
            if (gamepad.buttons[index]) {
                this.lastButtonState[index] = gamepad.buttons[index].pressed;
            }
        });

        // --- 2. Process Button Releases --- 
        const buttonMapping = { // Map index to state key
            0: 'a', 1: 'b', 4: 'lb', 6: 'lt', 7: 'rt', 8: 'share', 9: 'start',
            12: 'up', 13: 'down', 14: 'left', 15: 'right'
        };

        for (const indexStr in buttonMapping) {
            const index = parseInt(indexStr, 10);
            const stateKey = buttonMapping[index];
            if (gamepad.buttons[index] && !gamepad.buttons[index].pressed && this.buttonStates[stateKey]) {
                console.log(`[Gamepad] Button ${stateKey} (index ${index}) RELEASED. Internal state -> false.`);
                this.buttonStates[stateKey] = false; // FIX: use bracket notation instead of dot notation
                
                // Ensure both internal and persistent state are cleared
                this.lastButtonState[index] = false;
                
                // Reset the click debounce flag immediately on key release
                if (stateKey === 'a') {
                    console.log('[Gamepad] Resetting click debounce flag on key release.');
                    this.isClicking = false;
                }
                
                // No need to update session storage on every release - will be saved in stopGamepadPolling
            }
        }

        // --- 3. Process Button Presses ---
        // A button (Confirm / Click)
        if (gamepad.buttons[0].pressed && !this.buttonStates.a) {
            // New press detected
            if (this.isClicking) {
                console.log('[Gamepad] Debouncing A press.');
                return; 
            }
            this.isClicking = true; // Set debounce flag
            console.log('[Gamepad] Button A PRESSED (detected new press, setting state true).');
            this.buttonStates.a = true; // Set state immediately
            
            if (this.activeElement) {
                console.log('[Gamepad] Simulating click on active element:', this.activeElement);
                
                // Store information about this click in sessionStorage
                try {
                    sessionStorage.setItem('gamepadLastPressedButton', 'a');
                } catch (e) {
                    console.error('[Gamepad] Error storing press info:', e);
                }
                
                this.resetButtonStates(); // Reset states without stopping polling
                this.activeElement.click(); // Navigate
            } else {
                console.log('[Gamepad] A pressed, but no active element to click.');
                // State is already set true above
            }
        }
        
        // B button (Back/Cancel)
        if (gamepad.buttons[1].pressed && !this.buttonStates.b) {
            console.log('[Gamepad] Button B PRESSED (detected new press).');
            this.buttonStates.b = true;
            const modalVisible = document.querySelector('div[aria-modal="true"]:not([x-cloak])');
            if (modalVisible) {
                const cancelButton = modalVisible.querySelector('button:last-child'); // Consider a more specific selector if needed
                if (cancelButton) {
                    console.log('B button pressed - closing modal');
                    cancelButton.click();
                } else {
                     console.log('B button pressed in modal, no cancel button found, trying history back');
                     window.history.back(); // Go back if no explicit cancel found
                }
            } else {
                console.log('B button pressed - going back');
                window.history.back();
            }
        }
        
        // D-pad Up
        if (gamepad.buttons[12]?.pressed && !this.buttonStates.up) {
            console.log('[Gamepad] D-Pad Up PRESSED (detected new press).');
            this.buttonStates.up = true;
            this.navigateUp();
        }
        // D-pad Down
        if (gamepad.buttons[13]?.pressed && !this.buttonStates.down) {
            console.log('[Gamepad] D-Pad Down PRESSED (detected new press).');
            this.buttonStates.down = true;
            this.navigateDown();
        }
        // D-pad Left
        if (gamepad.buttons[14]?.pressed && !this.buttonStates.left) {
            console.log('[Gamepad] D-Pad Left PRESSED (detected new press).');
            this.buttonStates.left = true;
            this.navigateLeft();
        }
        // D-pad Right
        if (gamepad.buttons[15]?.pressed && !this.buttonStates.right) {
            console.log('[Gamepad] D-Pad Right PRESSED (detected new press).');
            this.buttonStates.right = true;
            this.navigateRight();
        }
        
        // LB (Theme Toggle)
        if (gamepad.buttons[4]?.pressed && !this.buttonStates.lb) {
            console.log('[Gamepad] Button LB PRESSED (detected new press).');
            this.buttonStates.lb = true;
            
            // Check if we're already toggling theme
            if (this.isTogglingTheme) {
                console.log('[Gamepad] Debouncing theme toggle.');
                return;
            }
            
            this.toggleTheme();
        }
        
        // LT (Previous Tab)
        if (gamepad.buttons[6]?.pressed && !this.buttonStates.lt) {
            console.log('[Gamepad] Button LT PRESSED (detected new press).');
            this.buttonStates.lt = true;
            this.switchTab('previous');
        }
        
        // RT (Next Tab)
        if (gamepad.buttons[7]?.pressed && !this.buttonStates.rt) {
            console.log('[Gamepad] Button RT PRESSED (detected new press).');
            this.buttonStates.rt = true;
            this.switchTab('next');
        }
        
        // Share/View Button (Fullscreen)
        if (gamepad.buttons[8]?.pressed && !this.buttonStates.share) {
            console.log('[Gamepad] Button Share PRESSED (detected new press).');
            this.buttonStates.share = true;
            this.toggleFullscreen();
        }
        
        // Start/Menu Button (Settings)
        if (gamepad.buttons[9]?.pressed && !this.buttonStates.start) {
            console.log('[Gamepad] Button Start PRESSED (detected new press).');
            this.buttonStates.start = true;
            const settingsLink = document.querySelector('a[href*="/settings"], button[aria-label*="Settings"]'); 
            if (settingsLink) {
                console.log('[Gamepad] Simulating click on Settings link...');
                this.resetButtonStates();   // Reset states without stopping polling
                settingsLink.click();
            } else {
                console.warn('[Gamepad] Could not find settings link/button.');
            }
        }

        // --- 4. Analog Stick Processing ---
        // Left analog stick
        const leftX = gamepad.axes[0];
        const leftY = gamepad.axes[1];
        if (Math.abs(leftX) > 0.5 || Math.abs(leftY) > 0.5) {
            if (!this.analogMoved) {
                this.analogMoved = true;
                if (leftX < -0.5) this.navigateLeft();
                else if (leftX > 0.5) this.navigateRight();
                if (leftY < -0.5) this.navigateUp();
                else if (leftY > 0.5) this.navigateDown();
            }
        } else {
            this.analogMoved = false;
        }

        // Right analog stick (Free Cursor)
        const rightX = gamepad.axes[2];
        const rightY = gamepad.axes[3];
        if (Math.abs(rightX) > DEADZONE || Math.abs(rightY) > DEADZONE) {
            this.updateFreeCursor(gamepad, rightX, rightY);
            this.showFreeCursor();
        } else if (this.freeCursorVisible) {
            // Stick is neutral, potentially start hide timer (handled in showFreeCursor)
        }
    }
    
    navigateUp() {
        const prevIndex = this.findAdjacentElement('up');
        if (prevIndex !== -1 && prevIndex !== this.currentElementIndex) {
            this.focusElementAtIndex(prevIndex);
        }
    }
    
    navigateDown() {
        const nextIndex = this.findAdjacentElement('down');
        if (nextIndex !== -1 && nextIndex !== this.currentElementIndex) {
            this.focusElementAtIndex(nextIndex);
        }
    }
    
    navigateLeft() {
        const leftIndex = this.findAdjacentElement('left');
        if (leftIndex !== -1 && leftIndex !== this.currentElementIndex) {
            this.focusElementAtIndex(leftIndex);
        }
    }
    
    navigateRight() {
        const rightIndex = this.findAdjacentElement('right');
        if (rightIndex !== -1 && rightIndex !== this.currentElementIndex) {
            this.focusElementAtIndex(rightIndex);
        }
    }
    
    findAdjacentElement(direction) {
        if (!this.activeElement || this.focusableElements.length === 0) return -1;
        
        // If we're in a modal, only navigate between elements in that modal
        const modalVisible = document.querySelector('div[aria-modal="true"]:not([x-cloak])');
        const currentInModal = modalVisible && modalVisible.contains(this.activeElement);
        
        const currentRect = this.activeElement.getBoundingClientRect();
        const currentCenterX = currentRect.left + currentRect.width / 2;
        const currentCenterY = currentRect.top + currentRect.height / 2;
        
        let bestIndex = -1;
        let bestDistance = Number.POSITIVE_INFINITY;
        
        // Check all focusable elements
        this.focusableElements.forEach((element, index) => {
            if (element === this.activeElement) return;
            
            // If we're in a modal, only navigate to elements also in that modal
            if (currentInModal && (!modalVisible || !modalVisible.contains(element))) {
                return;
            }
            
            const rect = element.getBoundingClientRect();
            const centerX = rect.left + rect.width / 2;
            const centerY = rect.top + rect.height / 2;
            
            // Check if element is in the right direction
            let inRightDirection = false;
            switch (direction) {
                case 'up':
                    inRightDirection = centerY < currentCenterY;
                    break;
                case 'down':
                    inRightDirection = centerY > currentCenterY;
                    break;
                case 'left':
                    inRightDirection = centerX < currentCenterX;
                    break;
                case 'right':
                    inRightDirection = centerX > currentCenterX;
                    break;
            }
            
            if (inRightDirection) {
                // Calculate distance based on direction priority
                let distance;
                
                if (direction === 'up' || direction === 'down') {
                    // For up/down prioritize vertical distance
                    distance = Math.abs(centerY - currentCenterY) * 3 + Math.abs(centerX - currentCenterX);
                } else {
                    // For left/right prioritize horizontal distance
                    distance = Math.abs(centerX - currentCenterX) * 3 + Math.abs(centerY - currentCenterY);
                }
                
                if (distance < bestDistance) {
                    bestDistance = distance;
                    bestIndex = index;
                }
            }
        });
        
        return bestIndex;
    }
    
    clearFocusStyles() {
        document.querySelectorAll('.gamepad-focus').forEach(el => {
            el.classList.remove('gamepad-focus');
        });
    }
    
    findTopTabs() {
        // Attempt to find the main navigation tabs. Adjust selector if needed.
        // Example: Assuming tabs are links directly within a <nav> inside the <header>
        const navElement = document.querySelector('header nav'); 
        if (navElement) {
            this.topNavTabs = Array.from(navElement.querySelectorAll('a'));
            console.log('Found top nav tabs:', this.topNavTabs);
        } else {
            console.warn('Could not find top navigation tabs container (header nav). Tab switching might not work.');
        }
    }
    
    toggleTheme() {
        // Only toggle if LB is pressed AND wasn't already held down in the previous frame
        if (!this.buttonStates.lb || this.lbButtonHeldDown) {
            return; 
        }

        // Mark LB as held down for this cycle to prevent repeats until released
        this.lbButtonHeldDown = true; 

        console.log('[Gamepad] Toggling theme');
        // Actual theme toggle logic
        const htmlElement = document.documentElement;
        if (htmlElement.classList.contains('dark')) {
            htmlElement.classList.remove('dark');
            localStorage.setItem('theme', 'light');
        } else {
            htmlElement.classList.add('dark');
            localStorage.setItem('theme', 'dark');
        }

        // No need for setTimeout or isTogglingTheme flag here anymore
    }
    
    toggleFullscreen() {
        if (!document.fullscreenElement) {
            document.documentElement.requestFullscreen().catch(err => {
                console.error(`Error attempting to enable fullscreen mode: ${err.message} (${err.name})`);
            });
        } else {
            if (document.exitFullscreen) {
                document.exitFullscreen();
            }
        }
    }
    
    switchTab(direction) {
        if (this.topNavTabs.length === 0) return;
        
        // Find the currently active tab (heuristic: has aria-current or a specific 'active' class)
        let currentIndex = this.topNavTabs.findIndex(tab => 
            tab.getAttribute('aria-current') === 'page' || tab.classList.contains('active-tab-class') // Adjust 'active-tab-class' if needed
        );
        
        if (currentIndex === -1) { // If no active tab found, maybe default to first or focused? For now, start from 0.
          currentIndex = 0; 
        }
        
        let nextIndex;
        if (direction === 'next') {
            nextIndex = (currentIndex + 1) % this.topNavTabs.length;
        } else { // 'previous'
            nextIndex = (currentIndex - 1 + this.topNavTabs.length) % this.topNavTabs.length;
        }
        
        // Simulate click on the next/previous tab
        const targetTab = this.topNavTabs[nextIndex];
        if (targetTab) {
            console.log(`Switching tab ${direction} to:`, targetTab);
            this.resetButtonStates();   // Reset states without stopping polling
            targetTab.click();
        }
    }
    
    updateFreeCursor(gamepad, axisX, axisY) {
        let currentSensitivity = 50; // Increased base sensitivity
        
        // Check if Right Trigger (button 7) is held for boost
        if (gamepad.buttons[7] && gamepad.buttons[7].pressed) {
            currentSensitivity *= 3; // Apply boost multiplier
        }
        
        this.freeCursorPosition.x += axisX * currentSensitivity;
        this.freeCursorPosition.y += axisY * currentSensitivity;
        
        // Clamp cursor position to screen bounds
        this.freeCursorPosition.x = Math.max(0, Math.min(window.innerWidth - 24, this.freeCursorPosition.x)); // 24 is cursor width
        this.freeCursorPosition.y = Math.max(0, Math.min(window.innerHeight - 24, this.freeCursorPosition.y)); // 24 is cursor height
        
        this.freeCursorElement.style.transform = `translate(${this.freeCursorPosition.x}px, ${this.freeCursorPosition.y}px)`;
    }
    
    showFreeCursor() {
        clearTimeout(this.freeCursorHideTimer); // Clear any existing hide timer
        this.freeCursorElement.style.display = 'flex'; // Use 'flex' due to centering styles
        this.freeCursorVisible = true;
        // Set a timer to hide the cursor after a period of inactivity (e.g., 3 seconds)
        this.freeCursorHideTimer = setTimeout(() => {
            this.freeCursorElement.style.display = 'none';
            this.freeCursorVisible = false;
        }, 3000); 
    }
    
    // Add a new method to reset button states without stopping polling
    resetButtonStates() {
        console.log('[Gamepad] Resetting button states without stopping polling.');
        
        // Save the last physical button state to session storage before reset
        try {
            sessionStorage.setItem('gamepadLastButtonState', JSON.stringify(this.lastButtonState));
        } catch (e) {
            console.error('[Gamepad] Error saving last button state:', e);
        }
        
        this.buttonStates = {}; // Clear states
        this.isClicking = false; // Clear debounce flag
        // Don't reset theme toggle debounce - that needs to persist across pages
    }
}

// Initialize the gamepad controller when the script loads
document.addEventListener('DOMContentLoaded', () => {
    // Delay initialization slightly to potentially help with element finding
    setTimeout(() => { window.gamepadController = new GamepadController(); }, 150);
}); 