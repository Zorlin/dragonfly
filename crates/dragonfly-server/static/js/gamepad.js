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
        this.lastAddMachineModalState = false; // Track Add Machine modal specifically
        this.freeCursorElement = null;
        this.freeCursorVisible = false;
        this.freeCursorHideTimer = null;
        this.freeCursorPosition = { x: window.innerWidth / 2, y: window.innerHeight / 2 }; // Start in center
        this.DEADZONE = 0.15; // Define the deadzone value
        this.isClicking = false; // ADD flag to debounce clicks
        this.isTogglingTheme = false; // Add flag to debounce theme toggle
        this.aButtonHeldDown = false; // Add this flag, initialized to false
        this.lbButtonHeldDown = false; // Flag for Left Bumper
        this.menuActive = false; // Flag to track if the gamepad menu is open
        
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
            
            // Also clear the gamepad detection interval if it's running
            if (this.gamepadDetectionInterval) {
                clearInterval(this.gamepadDetectionInterval);
                this.gamepadDetectionInterval = null;
            }
            
            // Hide any gamepad UI elements
            this.hideGamepadConnectPrompt();
            
            this.buttonStates = {}; // Explicitly clear states
        });
        
        // Listen for gamepad menu state changes
        window.addEventListener('gamepad-menu-active', (event) => {
            console.log('[Gamepad] Menu active state changed:', event.detail.active);
            this.menuActive = event.detail.active;
            
            // Clear button states when menu state changes
            this.buttonStates = {};
        });
        
        // Add window focus events to re-check for gamepads
        window.addEventListener('focus', () => {
            console.log('[Gamepad] Window focused, checking for gamepads...');
            this.checkForExistingGamepads();
        });
        
        // Also check for gamepads on user interactions
        document.addEventListener('click', () => {
            if (!this.gamepadConnected) {
                this.checkForExistingGamepads();
            }
        });
        
        // Check for already connected gamepads
        setTimeout(() => this.checkForExistingGamepads(), 100);
        
        // Also start a recurring check every 2 seconds until a gamepad is found
        this.gamepadDetectionInterval = setInterval(() => {
            if (!this.gamepadConnected) {
                console.log('[Gamepad] Polling for connected gamepads...');
                this.checkForExistingGamepads();
                this.showGamepadConnectPrompt();
            } else {
                // Once gamepad is found, stop the detection interval
                clearInterval(this.gamepadDetectionInterval);
                this.hideGamepadConnectPrompt();
            }
        }, 2000);
        
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
            // Use improved modal detection logic
            let modalVisible = false;
            const possibleModals = document.querySelectorAll('div[aria-modal="true"], div[id="add-machine-modal"], div[class*="modal"]');
            
            for (const modal of possibleModals) {
                if (modal.hasAttribute('x-cloak') || 
                    window.getComputedStyle(modal).display === 'none' ||
                    window.getComputedStyle(modal).visibility === 'hidden') {
                    continue;
                }
                
                const rect = modal.getBoundingClientRect();
                if (rect.width > 0 && rect.height > 0) {
                    modalVisible = true;
                    
                    // Specially handle the Add Machine modal
                    if (modal.id === 'add-machine-modal' || 
                        modal.classList.contains('add-machine-modal') || 
                        modal.querySelector('[x-show="addMachineModal"]')) {
                        console.log('[Gamepad] Add Machine modal detected as active');
                        if (!this.lastAddMachineModalState) {
                            // Add Machine modal just opened
                            console.log('[Gamepad] Add Machine modal just opened, forcing focus update');
                            this.lastAddMachineModalState = true;
                            
                            // Force a more aggressive focus update with retries
                            setTimeout(() => {
                                this.updateFocusableElements();
                                const modalElements = this.getElementsInActiveModal();
                                console.log('[Gamepad] Found', modalElements.length, 'elements in Add Machine modal');
                                
                                if (modalElements.length > 0) {
                                    const firstModalElement = modalElements[0];
                                    const index = this.focusableElements.indexOf(firstModalElement);
                                    if (index !== -1) {
                                        console.log('[Gamepad] Focusing first element in Add Machine modal');
                                        this.focusElementAtIndex(index);
                                    }
                                }
                            }, 200);
                        }
                    }
                    
                    break;
                }
            }
            
            // If modal state changed
            if (modalVisible !== this.lastModalState) {
                this.lastModalState = modalVisible;
                
                // Reset Add Machine modal tracking if no modals are visible
                if (!modalVisible) {
                    this.lastAddMachineModalState = false;
                }
                
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
    
    showGamepadConnectPrompt() {
        // Only show the prompt if a gamepad isn't connected yet
        if (this.gamepadConnected) return;
        
        // Remove any existing prompt first
        this.hideGamepadConnectPrompt();
        
        // Create a prompt element
        const prompt = document.createElement('div');
        prompt.id = 'gamepad-connect-prompt';
        prompt.className = 'fixed top-4 right-4 bg-indigo-600 text-white p-3 rounded-lg text-sm z-[9999] animate-pulse';
        prompt.innerHTML = `
          <div class="flex items-center space-x-2">
            <span>🎮</span>
            <span>Gamepad detected! Press any button to activate</span>
          </div>
        `;
        
        // Check if any gamepad is present in navigator.getGamepads()
        const gamepads = navigator.getGamepads ? navigator.getGamepads() : [];
        let hasPhysicalGamepad = false;
        
        for (let i = 0; i < gamepads.length; i++) {
            // Some browsers report null for disconnected controller slots
            // but in some cases will show an entry with id="" for physically connected but inactive gamepads
            if (gamepads[i] && (gamepads[i].id !== "" || gamepads[i].connected)) {
                hasPhysicalGamepad = true;
                break;
            }
        }
        
        // Only show the prompt if a gamepad appears to be physically connected
        if (hasPhysicalGamepad) {
            document.body.appendChild(prompt);
        }
    }
    
    hideGamepadConnectPrompt() {
        const prompt = document.getElementById('gamepad-connect-prompt');
        if (prompt) prompt.remove();
    }
    
    checkForExistingGamepads() {
        const gamepads = navigator.getGamepads ? navigator.getGamepads() : [];
        console.log('Checking for existing gamepads:', gamepads);
        let foundOne = false;
        
        for (let i = 0; i < gamepads.length; i++) {
            // In some browsers, disconnected gamepad slots may show as null or with empty id
            if (gamepads[i] && gamepads[i].id !== "" && gamepads[i].connected) {
                console.log('Found existing active gamepad:', gamepads[i]);
                // Manually trigger the connection handler for the existing gamepad
                this.handleGamepadConnected({ gamepad: gamepads[i] });
                foundOne = true;
            } else if (gamepads[i] && gamepads[i].id !== "") {
                // If we have a gamepad with an ID but it's not marked as connected,
                // it might be physically connected but waiting for a button press
                console.log('Found potential gamepad that needs activation:', gamepads[i]);
                this.showGamepadConnectPrompt();
            }
        }
        
        // If any existing gamepad was found, dispatch the connected event
        if (foundOne) {
            window.dispatchEvent(new CustomEvent('gamepad-connection-changed', { detail: { connected: true } }));
        }
        
        return foundOne;
    }
    
    getElementsInActiveModal() {
        // Use improved modal detection logic
        let visibleModal = null;
        const possibleModals = document.querySelectorAll('div[aria-modal="true"], div[id="add-machine-modal"], div[class*="modal"]');
        
        for (const modal of possibleModals) {
            if (modal.hasAttribute('x-cloak') || 
                window.getComputedStyle(modal).display === 'none' ||
                window.getComputedStyle(modal).visibility === 'hidden') {
                continue;
            }
            
            const rect = modal.getBoundingClientRect();
            if (rect.width > 0 && rect.height > 0) {
                visibleModal = modal;
                
                // Special detection for Add Machine modal
                if (modal.id === 'add-machine-modal' || 
                    modal.classList.contains('add-machine-modal') || 
                    modal.querySelector('[x-show="addMachineModal"]')) {
                    console.log('[Gamepad] Add Machine modal is the active modal for element selection');
                }
                
                break;
            }
        }
        
        if (!visibleModal) return [];
        
        console.log('[Gamepad] Found visible modal for element selection:', visibleModal.id || visibleModal.className);
        
        // Get all focusable elements within that modal
        const modalElements = this.focusableElements.filter(el => visibleModal.contains(el));
        
        // Special handling for Add Machine modal
        if (visibleModal.id === 'add-machine-modal' || 
            visibleModal.classList.contains('add-machine-modal') || 
            visibleModal.querySelector('[x-show="addMachineModal"]')) {
            
            // Explicitly look for card options in the Add Machine modal
            const cardOptions = Array.from(visibleModal.querySelectorAll('.grid > div, .grid button, .card, div.rounded-lg, button.rounded-lg, [x-on\\:click*="reimage"], [x-on\\:click*="open"]'));
            
            console.log('[Gamepad] Found', cardOptions.length, 'potential card options in Add Machine modal');
            
            // Make sure we have at least found some elements
            if (modalElements.length === 0 && cardOptions.length > 0) {
                // Add these card options to focusableElements if not already there
                cardOptions.forEach(card => {
                    if (!this.focusableElements.includes(card) && 
                        !card.classList.contains('gamepad-nav-exclude') &&
                        window.getComputedStyle(card).display !== 'none' && 
                        window.getComputedStyle(card).visibility !== 'hidden') {
                        
                        console.log('[Gamepad] Adding card option to focusable elements:', card);
                        this.focusableElements.push(card);
                    }
                });
                
                // Refilter to include the new elements
                return this.focusableElements.filter(el => visibleModal.contains(el));
            }
        }
        
        return modalElements;
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
        
        // Clear the detection interval now that we have a connected gamepad
        if (this.gamepadDetectionInterval) {
            clearInterval(this.gamepadDetectionInterval);
            this.gamepadDetectionInterval = null;
        }
        
        // Hide the connection prompt if it's visible
        this.hideGamepadConnectPrompt();
        
        // Show the gamepad UI
        this.showGamepadUI();
        this.startGamepadPolling();
        
        // After a short delay, try to focus on the first row
        setTimeout(() => {
            const machineRows = document.querySelectorAll('tr[data-machine-id]');
            if (machineRows.length > 0) {
                this.updateFocusableElements(); // Refresh focusable elements
                
                const firstRowIndex = this.focusableElements.findIndex(el => 
                    el.tagName === 'TR' && el.hasAttribute('data-machine-id')
                );
                
                if (firstRowIndex !== -1) {
                    console.log('[Gamepad] Focusing first machine row after controller connection');
                    this.focusElementAtIndex(firstRowIndex);
                }
            }
        }, 500);
        
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
                    background-color: rgba(99, 102, 241, 0.15) !important;
                }
                
                button.gamepad-focus, a.gamepad-focus {
                    transform: scale(1.05);
                    outline-offset: 3px !important;
                    box-shadow: 0 0 12px rgba(129, 140, 248, 0.8);
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
        
        // Focus the first element with a retry mechanism
        setTimeout(() => {
            // Force an update of focusable elements again
            this.updateFocusableElements();
            
            // Check if we're on the machine list page
            const machineRows = document.querySelectorAll('tr[data-machine-id]');
            if (machineRows.length > 0) {
                // We're on the machine list page, focus the first row
                console.log('[Gamepad] Detected machine list page, focusing first row');
                
                // Find the first row in our focusable elements
                const firstRowIndex = this.focusableElements.findIndex(el => 
                    el.tagName === 'TR' && el.hasAttribute('data-machine-id')
                );
                
                if (firstRowIndex !== -1) {
                    console.log('[Gamepad] Found first row at index', firstRowIndex);
                    this.focusElementAtIndex(firstRowIndex);
                    return;
                }
            } 
            
            // Fallback to normal behavior if not on machine list or no rows found
            if (this.focusableElements.length > 0) {
                // Avoid focusing the Add Machine button as first element
                const addMachineButtonIndex = this.focusableElements.findIndex(el => {
                    return (el.textContent && el.textContent.includes('Add Machine')) || 
                           (el.innerText && el.innerText.includes('Add Machine'));
                });
                
                if (addMachineButtonIndex === 0 && this.focusableElements.length > 1) {
                    console.log('[Gamepad] Skipping Add Machine button as first focus, using index 1 instead');
                    this.focusElementAtIndex(1);
                } else {
                    console.log('Attempting initial focus (index 0) with delay...');
                    this.focusElementAtIndex(0);
                }
            } else {
                console.warn('No focusable elements found when showing gamepad UI, retrying...');
                // Try again after a longer delay
                setTimeout(() => {
                    this.updateFocusableElements();
                    
                    // Try finding machine rows again
                    const machineRows = document.querySelectorAll('tr[data-machine-id]');
                    if (machineRows.length > 0) {
                        const firstRowIndex = this.focusableElements.findIndex(el => 
                            el.tagName === 'TR' && el.hasAttribute('data-machine-id')
                        );
                        
                        if (firstRowIndex !== -1) {
                            console.log('[Gamepad] Found first row at index', firstRowIndex);
                            this.focusElementAtIndex(firstRowIndex);
                            return;
                        }
                    }
                    
                    // Last fallback - use the first element, but avoid Add Machine button
                    if (this.focusableElements.length > 0) {
                        // Check again if the first element is the Add Machine button
                        const addMachineButtonIndex = this.focusableElements.findIndex(el => {
                            return (el.textContent && el.textContent.includes('Add Machine')) || 
                                   (el.innerText && el.innerText.includes('Add Machine'));
                        });
                        
                        if (addMachineButtonIndex === 0 && this.focusableElements.length > 1) {
                            console.log('[Gamepad] Skipping Add Machine button as first focus, using index 1 instead');
                            this.focusElementAtIndex(1);
                        } else {
                            console.log('Second attempt at focusing element 0...');
                            this.focusElementAtIndex(0);
                        }
                    } else {
                        console.error('Still no focusable elements found after retry.');
                    }
                }, 500);
            }
        }, 200);
        
        // Show gamepad controls hint
        const hint = document.createElement('div');
        hint.id = 'gamepad-hint';
        hint.className = 'gamepad-nav-exclude fixed bottom-4 left-4 bg-black bg-opacity-70 text-white p-3 rounded-lg text-sm z-[9999]';
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
    
    // Function to log debuggable information about element selections
    logElementSelectionDebug() {
        console.log('====== GAMEPAD ELEMENT SELECTION DEBUG ======');
        // Count all potential elements
        const allLinks = document.querySelectorAll('a');
        const allButtons = document.querySelectorAll('button');
        const allInputs = document.querySelectorAll('input');
        const machineRows = document.querySelectorAll('tr[data-machine-id]');
        const osDropdowns = document.querySelectorAll('.flex.items-center.cursor-pointer');
        
        console.log(`Found ${allLinks.length} links, ${allButtons.length} buttons, ${allInputs.length} inputs, ${machineRows.length} machine rows, ${osDropdowns.length} OS dropdowns`);
        
        // Check if they're visible
        const visibleLinks = Array.from(allLinks).filter(el => {
            const style = window.getComputedStyle(el);
            return style.display !== 'none' && style.visibility !== 'hidden';
        });
        const visibleButtons = Array.from(allButtons).filter(el => {
            const style = window.getComputedStyle(el);
            return style.display !== 'none' && style.visibility !== 'hidden';
        });
        
        console.log(`Of which ${visibleLinks.length} links and ${visibleButtons.length} buttons are visible`);
        console.log('==========================================');
    }
    
    updateFocusableElements() {
        // Debug output
        this.logElementSelectionDebug();
        
        // Check if a modal is active first - ENHANCE this to only detect VISIBLE modals
        const possibleModals = document.querySelectorAll('div[aria-modal="true"], div[id="add-machine-modal"], div[class*="modal"]');
        
        // More careful verification that modal is actually visible
        let modalVisible = null;
        for (const modal of possibleModals) {
            // Skip if it has x-cloak or display:none
            if (modal.hasAttribute('x-cloak') || 
                window.getComputedStyle(modal).display === 'none' ||
                window.getComputedStyle(modal).visibility === 'hidden') {
                continue;
            }
            
            // Ensure it's actually visible in the viewport
            const rect = modal.getBoundingClientRect();
            if (rect.width > 0 && rect.height > 0) {
                console.log('[Gamepad] Found visible modal:', modal);
                modalVisible = modal;
                break;
            }
        }
        
        // If no visible modal was found, log it
        if (!modalVisible) {
            console.log('[Gamepad] No visible modal detected');
        }
        
        // Less restrictive selection - get ALL interactive elements
        let allFocusableElements = [];
        
        // Get links, buttons, and form elements
        const interactiveSelectors = [
            'a:not(.gamepad-nav-exclude)', 
            'button:not(.gamepad-nav-exclude)', 
            'select:not(.gamepad-nav-exclude)', 
            'input:not(.gamepad-nav-exclude)', 
            'textarea:not(.gamepad-nav-exclude)', 
            '[tabindex]:not([tabindex="-1"]):not(.gamepad-nav-exclude)',
            '.nav-link:not(.gamepad-nav-exclude)', 
            '.btn:not(.gamepad-nav-exclude)', 
            '[role="button"]:not(.gamepad-nav-exclude)',
            '.card:not(.gamepad-nav-exclude)',
            '.rounded-lg:not(.gamepad-nav-exclude)'
        ];
        
        // First, gather all possible elements
        interactiveSelectors.forEach(selector => {
            const elements = document.querySelectorAll(selector);
            allFocusableElements = [...allFocusableElements, ...Array.from(elements)];
        });
        
        // Remove duplicates
        allFocusableElements = [...new Set(allFocusableElements)];
        
        // Specifically find action buttons in machine rows
        const actionButtons = [];
        const machineRowsActions = document.querySelectorAll('tr[data-machine-id] td:last-child button, tr[data-machine-id] td:last-child a[href]');
        console.log(`[Gamepad] Specifically found ${machineRowsActions.length} action buttons in machine rows`);
        if (machineRowsActions.length > 0) {
            // Add these to focusable elements separately to ensure they're included
            machineRowsActions.forEach(button => {
                if (!button.classList.contains('gamepad-nav-exclude')) {
                    actionButtons.push(button);
                }
            });
        }
        
        // Explicitly check for machine rows, which are important for navigation
        const machineRows = Array.from(document.querySelectorAll('tr[data-machine-id]'));
        console.log(`[Gamepad] Specifically found ${machineRows.length} machine rows`);
        
        // Find OS dropdown triggers
        const osDropdownTriggers = Array.from(document.querySelectorAll('div.flex.items-center.cursor-pointer, div[class*="cursor-pointer"][x-on\\:click*="toggleOsDropdown"], [class*="cursor-pointer"][x-on\\:click*="osDropdowns"], [class*="cursor-pointer"][data-dropdown]'));
        console.log(`[Gamepad] Found ${osDropdownTriggers.length} OS dropdown triggers`);
        
        // Find OS dropdown options (both open and closed)
        const osDropdownOptions = Array.from(document.querySelectorAll('div[x-show*="osDropdowns"] a, div[x-show*="osDropdown"] a[href="#"]'));
        console.log(`[Gamepad] Found ${osDropdownOptions.length} OS dropdown options`);
        
        console.log(`[Gamepad] Found ${allFocusableElements.length} total interactive elements before filtering`);
        
        // Filter for visibility but be less strict
        allFocusableElements = allFocusableElements.filter(el => {
            if (!el || !el.style) return false;
            
            try {
                const style = window.getComputedStyle(el);
                return style.display !== 'none' && 
                       style.visibility !== 'hidden' && 
                       !el.hasAttribute('disabled') &&
                       !el.hasAttribute('aria-hidden');
            } catch (e) {
                console.warn('[Gamepad] Error checking element style:', e);
                return false;
            }
        });
        
        console.log(`[Gamepad] ${allFocusableElements.length} elements remain after visibility filtering`);
        
        // Filter out excluded elements - do this BEFORE filtering for modals
        const preExcludeCount = allFocusableElements.length;
        allFocusableElements = allFocusableElements.filter(el => {
            try {
                return !el.classList.contains('gamepad-nav-exclude');
            } catch (e) {
                // If error in closest(), keep the element
                console.warn('[Gamepad] Error checking gamepad-nav-exclude:', e);
                return true;
            }
        });
        console.log(`[Gamepad] Removed ${preExcludeCount - allFocusableElements.length} elements with gamepad-nav-exclude class`);
        
        // Filter machine rows too
        const visibleMachineRows = machineRows.filter(row => {
            // Make sure it's visible
            try {
                const style = window.getComputedStyle(row);
                // Make sure it doesn't have the exclude class
                return style.display !== 'none' && 
                       style.visibility !== 'hidden' && 
                       !row.classList.contains('gamepad-nav-exclude');
            } catch (e) {
                return false;
            }
        });
        console.log(`[Gamepad] After filtering, ${visibleMachineRows.length} machine rows remain`);
        
        // Special handling for Add Machine modal
        const addMachineModal = modalVisible && (
            modalVisible.id === 'add-machine-modal' || 
            modalVisible.classList.contains('add-machine-modal') || 
            modalVisible.getAttribute('x-show') === 'addMachineModal' ||
            modalVisible.querySelector('[x-show="addMachineModal"]')
        );
        
        // If modal is visible, only include elements inside the modal
        if (modalVisible) {
            this.focusableElements = allFocusableElements.filter(el => modalVisible.contains(el));
            console.log(`[Gamepad] Modal active: filtered to ${this.focusableElements.length} elements inside modal`);
            
            // Specifically for add-machine-modal, make sure we include the card options
            if (addMachineModal) {
                console.log('[Gamepad] Special handling for Add Machine modal');
                
                // For machine cards, grab all div and button elements that look like cards
                const cardSelectors = [
                    '.grid > div[class*="cursor-pointer"]',
                    '.grid > div.rounded-lg',
                    '.grid > button.rounded-lg',
                    '.grid > div',
                    'div.card',
                    'div.rounded-lg',
                    'button.rounded-lg',
                    '[x-on\\:click*="externalMachineModal"]',
                    '[x-on\\:click*="addPxeModal"]',
                    '[x-on\\:click*="bmcMachineModal"]'
                ];
                
                // Join all selectors with commas
                const combinedSelector = cardSelectors.join(', ');
                const addMachineCards = Array.from(modalVisible.querySelectorAll(combinedSelector));
                
                console.log(`[Gamepad] Found ${addMachineCards.length} machine cards in Add Machine modal`);
                
                // Add all cards to focusable elements
                addMachineCards.forEach(card => {
                    if (!this.focusableElements.includes(card) && 
                        window.getComputedStyle(card).display !== 'none' && 
                        window.getComputedStyle(card).visibility !== 'hidden') {
                        console.log('[Gamepad] Adding machine card to focusable elements:', card);
                        this.focusableElements.push(card);
                    }
                });
                
                // Also include direct div children that look like cards
                const cardOptions = Array.from(modalVisible.querySelectorAll('div > div.rounded-lg, button.rounded-lg, .grid > div'));
                cardOptions.forEach(card => {
                    if (!this.focusableElements.includes(card) && 
                        window.getComputedStyle(card).display !== 'none' && 
                        window.getComputedStyle(card).visibility !== 'hidden') {
                        this.focusableElements.push(card);
                    }
                });
                
                console.log(`[Gamepad] After adding cards: ${this.focusableElements.length} elements`);
                
                // If we still don't have any elements, try a more aggressive approach
                if (this.focusableElements.length === 0) {
                    console.log('[Gamepad] Still no elements found, using aggressive selection for Add Machine modal');
                    
                    // Include ANY clickable element
                    const allClickableElements = Array.from(modalVisible.querySelectorAll('*'))
                        .filter(el => {
                            // Only include visible elements
                            try {
                                const style = window.getComputedStyle(el);
                                if (style.display === 'none' || style.visibility === 'hidden') {
                                    return false;
                                }
                                
                                // Check for potential clickable traits
                                return (
                                    el.tagName === 'BUTTON' ||
                                    el.tagName === 'A' ||
                                    el.tagName === 'INPUT' ||
                                    el.getAttribute('role') === 'button' ||
                                    el.classList.contains('rounded-lg') ||
                                    el.classList.contains('card') ||
                                    el.classList.contains('cursor-pointer') ||
                                    el.getAttribute('x-on:click') ||
                                    el.getAttribute('@click')
                                );
                            } catch (e) {
                                return false;
                            }
                        });
                    
                    console.log(`[Gamepad] Found ${allClickableElements.length} potentially clickable elements`);
                    
                    // Add them all to focusable elements
                    allClickableElements.forEach(el => {
                        if (!this.focusableElements.includes(el)) {
                            this.focusableElements.push(el);
                        }
                    });
                    
                    console.log(`[Gamepad] After aggressive selection: ${this.focusableElements.length} elements`);
                }
            }
        } else {
            // If no modal is visible, include all document elements
            this.focusableElements = [...allFocusableElements];
            
            // Include machine list rows when no modal is visible - USE VISIBLE MACHINE ROWS
            if (visibleMachineRows.length) {
                console.log(`[Gamepad] Found ${visibleMachineRows.length} visible machine rows to add`);
                
                // Log details about the first machine row to help debugging
                if (visibleMachineRows.length > 0) {
                    const firstRow = visibleMachineRows[0];
                    console.log('[Gamepad] First machine row details:', {
                        id: firstRow.getAttribute('data-machine-id'),
                        className: firstRow.className,
                        display: window.getComputedStyle(firstRow).display,
                        childNodes: firstRow.childNodes.length
                    });
                }
                
                // Add visible machine rows
                visibleMachineRows.forEach(row => {
                    if (!this.focusableElements.includes(row)) {
                        console.log(`[Gamepad] Adding machine row ${row.getAttribute('data-machine-id')} to focusable elements`);
                        this.focusableElements.push(row);
                    } else {
                        console.log(`[Gamepad] Machine row ${row.getAttribute('data-machine-id')} already in focusable elements`);
                    }
                });
                
                // Add the action buttons we found earlier
                actionButtons.forEach(button => {
                    if (!this.focusableElements.includes(button)) {
                        console.log(`[Gamepad] Adding action button to focusable elements:`, button);
                        this.focusableElements.push(button);
                    }
                });
                
                // Add OS dropdown triggers
                osDropdownTriggers.forEach(trigger => {
                    if (!this.focusableElements.includes(trigger) && 
                        !trigger.classList.contains('gamepad-nav-exclude')) {
                        console.log(`[Gamepad] Adding OS dropdown trigger to focusable elements:`, trigger);
                        this.focusableElements.push(trigger);
                    }
                });
                
                // Add visible OS dropdown options (only add options from open dropdowns)
                const visibleOsOptions = osDropdownOptions.filter(option => {
                    try {
                        // Check if this option is in a visible dropdown
                        const dropdown = option.closest('[x-show]');
                        if (!dropdown) return false;
                        
                        // Check if it's visible
                        const style = window.getComputedStyle(option);
                        return style.display !== 'none' && style.visibility !== 'hidden';
                    } catch (e) {
                        return false;
                    }
                });
                
                visibleOsOptions.forEach(option => {
                    if (!this.focusableElements.includes(option) && 
                        !option.classList.contains('gamepad-nav-exclude')) {
                        console.log(`[Gamepad] Adding OS dropdown option to focusable elements:`, option);
                        this.focusableElements.push(option);
                    }
                });
                
                console.log(`[Gamepad] Added machine rows, action buttons, and OS elements. Now have ${this.focusableElements.length} focusable elements`);
            } else {
                console.warn('[Gamepad] No visible machine rows found to add');
            }
        }
        
        // Filter out elements that are hidden by Alpine's x-cloak but be more careful
        const preXCloakCount = this.focusableElements.length;
        this.focusableElements = this.focusableElements.filter(el => {
            try {
                return !el.closest('[x-cloak]');
            } catch (e) {
                // If error in closest(), keep the element
                console.warn('[Gamepad] Error checking x-cloak:', e);
                return true;
            }
        });
        console.log(`[Gamepad] Removed ${preXCloakCount - this.focusableElements.length} elements with x-cloak`);

        // FALLBACK: If we have no focusable elements, try a much simpler approach
        if (this.focusableElements.length === 0) {
            console.warn('[Gamepad] No focusable elements found after filtering, using fallback selection');
            
            // Just get navigation links and buttons at minimum
            const navLinks = Array.from(document.querySelectorAll('nav a, .nav a, a[href^="/"]'))
                .filter(link => !link.classList.contains('gamepad-nav-exclude'));
            console.log(`[Gamepad] Fallback found ${navLinks.length} navigation links`);
            this.focusableElements = navLinks;
            
            // Add main navigation buttons if we can find any
            if (this.focusableElements.length === 0) {
                console.warn('[Gamepad] No nav links found in fallback, trying top level buttons');
                const topButtons = Array.from(document.querySelectorAll('header button, nav button'))
                    .filter(btn => !btn.classList.contains('gamepad-nav-exclude'));
                this.focusableElements = topButtons;
            }
            
            // Last resort: try to use machine rows
            if (this.focusableElements.length === 0 && visibleMachineRows.length > 0) {
                console.warn('[Gamepad] Using machine rows as last resort');
                this.focusableElements = [...visibleMachineRows];
            }
        }

        // Final log
        if (this.focusableElements.length === 0) {
            console.error('[Gamepad] CRITICAL: Still no focusable elements found after fallback!');
        } else {
            console.log(`[Gamepad] Final count: ${this.focusableElements.length} focusable elements found`);
            
            // Log the first few for debugging
            if (this.focusableElements.length > 0) {
                console.log('[Gamepad] First few elements:', this.focusableElements.slice(0, 3));
            }
        }
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
        
        // Check if this is a table row with machine ID
        const isMachineRow = this.activeElement.tagName === 'TR' && 
                          this.activeElement.hasAttribute('data-machine-id');
        
        if (isMachineRow) {
            console.log(`[Gamepad] Focusing machine row with ID: ${this.activeElement.getAttribute('data-machine-id')}`);
        }
        
        console.log('[Gamepad] Focusing element:', this.activeElement);
        
        // Scroll element into view if needed
        this.activeElement.scrollIntoView({ behavior: 'smooth', block: 'center' });
        
        // Clear any existing focus styles first
        this.clearFocusStyles();
        
        // Add focus styles
        this.activeElement.classList.add('gamepad-focus');
        
        // Check if we're in Add Machine modal - add additional highlight if it's a card
        const addMachineModal = document.getElementById('add-machine-modal');
        if (addMachineModal && !addMachineModal.classList.contains('hidden') && 
            addMachineModal.contains(this.activeElement)) {
            
            // If it's a card in the Add Machine modal, add extra highlight
            if (this.activeElement.classList.contains('rounded-lg') || 
                this.activeElement.querySelector('.rounded-lg')) {
                this.activeElement.classList.add('ring-2', 'ring-indigo-500', 'ring-offset-2');
            }
        }
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
        
        // If the menu is not active, ensure we have focus on something
        if (!this.menuActive) {
            // Force update elements if we don't have any
            if (this.focusableElements.length === 0) {
                console.log('[Gamepad] No focusable elements, forcing update...');
                this.updateFocusableElements();
            }
            
            // Ensure we have an active element
            if (!this.activeElement && this.focusableElements.length > 0) {
                console.log('[Gamepad] No active element but found focusable elements, establishing focus...');
                this.ensureFocus();
            }
            
            // If we still have no elements, log and exit early
            if (this.focusableElements.length === 0) {
                if (!this.hasLoggedNoElements) {
                    console.warn('[Gamepad] No focusable elements available for navigation, skipping input handling');
                    this.hasLoggedNoElements = true; // Log only once
                }
                return;
            } else {
                this.hasLoggedNoElements = false; // Reset so we can log again if elements disappear
            }
        }
        
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
        
        // Start Button - Open the menu
        if (gamepad.buttons[9]?.pressed && !this.buttonStates.start) {
            console.log('[Gamepad] Button Start PRESSED (detected new press).');
            this.buttonStates.start = true;
            
            // Dispatch menu open event
            window.dispatchEvent(new CustomEvent('gamepad-menu-open'));
            return; // Exit early to prevent other button processing
        }
        
        // If menu is active, dispatch button events to it
        if (this.menuActive) {
            // D-pad Up (Menu Navigation)
            if (gamepad.buttons[12]?.pressed && !this.buttonStates.up) {
                console.log('[Gamepad] Menu: D-Pad Up PRESSED');
                this.buttonStates.up = true;
                window.dispatchEvent(new CustomEvent('gamepad-button-press', { 
                    detail: { button: 'up' } 
                }));
            }
            
            // D-pad Down (Menu Navigation)
            if (gamepad.buttons[13]?.pressed && !this.buttonStates.down) {
                console.log('[Gamepad] Menu: D-Pad Down PRESSED');
                this.buttonStates.down = true;
                window.dispatchEvent(new CustomEvent('gamepad-button-press', { 
                    detail: { button: 'down' } 
                }));
            }
            
            // A Button (Menu Select)
            if (gamepad.buttons[0]?.pressed && !this.buttonStates.a) {
                console.log('[Gamepad] Menu: A Button PRESSED');
                this.buttonStates.a = true;
                window.dispatchEvent(new CustomEvent('gamepad-button-press', { 
                    detail: { button: 'a' } 
                }));
            }
            
            // B Button (Menu Close)
            if (gamepad.buttons[1]?.pressed && !this.buttonStates.b) {
                console.log('[Gamepad] Menu: B Button PRESSED');
                this.buttonStates.b = true;
                window.dispatchEvent(new CustomEvent('gamepad-button-press', { 
                    detail: { button: 'b' } 
                }));
            }
            
            // Skip regular gamepad input processing when menu is active
            return;
        }
        
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
                
                // Check if this is a machine row
                const isMachineRow = this.activeElement.tagName === 'TR' && 
                              this.activeElement.hasAttribute('data-machine-id');
                
                // Check if this is an OS dropdown trigger
                const isOsDropdownTrigger = this.activeElement.classList.contains('cursor-pointer') && 
                                      (this.activeElement.getAttribute('x-on:click')?.includes('toggleOsDropdown') ||
                                       this.activeElement.getAttribute('@click')?.includes('toggleOsDropdown') ||
                                       this.activeElement.getAttribute('x-on:click')?.includes('osDropdowns') ||
                                       this.activeElement.getAttribute('@click')?.includes('osDropdowns'));
                
                // Check if this is an OS dropdown option (inside an open dropdown)
                const isOsDropdownOption = this.activeElement.closest('[x-show*="osDropdowns"]') && 
                                     this.activeElement.tagName === 'A' && 
                                     this.activeElement.getAttribute('href') === '#';
                
                if (isOsDropdownTrigger) {
                    console.log('[Gamepad] Clicking on OS dropdown trigger');
                    
                    // For OS dropdown triggers, we need to click it to toggle the dropdown
                    this.resetButtonStates();
                    this.activeElement.click();
                    
                    // After clicking, we need to update focusable elements to include the dropdown options
                    setTimeout(() => {
                        this.updateFocusableElements();
                    }, 100);
                    
                    return;
                }
                
                if (isOsDropdownOption) {
                    console.log('[Gamepad] Clicking on OS dropdown option');
                    
                    // Store a reference to the parent row or dropdown trigger before clicking the option
                    const dropdownContainer = this.activeElement.closest('[x-show*="osDropdowns"]');
                    const parentRow = dropdownContainer?.closest('tr[data-machine-id]');
                    
                    // Try to find the dropdown trigger in this row
                    let dropdownTrigger = null;
                    if (parentRow) {
                        dropdownTrigger = parentRow.querySelector('.cursor-pointer');
                        console.log('[Gamepad] Found dropdown trigger to return focus to after selection:', dropdownTrigger);
                    }
                    
                    // For OS dropdown options, click to select that OS
                    this.resetButtonStates();
                    this.activeElement.click();
                    
                    // After clicking an option, we need to update focusable elements and return focus to the trigger
                    setTimeout(() => {
                        this.updateFocusableElements();
                        
                        // Return focus to the dropdown trigger if we found it
                        if (dropdownTrigger && this.focusableElements.includes(dropdownTrigger)) {
                            const triggerIndex = this.focusableElements.indexOf(dropdownTrigger);
                            console.log('[Gamepad] Returning focus to dropdown trigger at index:', triggerIndex);
                            this.focusElementAtIndex(triggerIndex);
                        }
                    }, 100);
                    
                    return;
                }
                
                if (isMachineRow) {
                    console.log('[Gamepad] Clicking on machine row with ID:', this.activeElement.getAttribute('data-machine-id'));
                    
                    // For machine rows, we need to navigate to the machine details
                    const machineId = this.activeElement.getAttribute('data-machine-id');
                    if (machineId) {
                        // Navigate to the machine details page
                        window.location.href = `/machines/${machineId}`;
                        return;
                    }
                }
                
                // Special handling for Add Machine modal cards
                const addMachineModal = document.getElementById('add-machine-modal');
                if (addMachineModal && !addMachineModal.classList.contains('hidden') && 
                    addMachineModal.contains(this.activeElement)) {
                    
                    // If it's not a button but a div card, find and click its button if it has one
                    if (this.activeElement.tagName === 'DIV' && !this.activeElement.classList.contains('button')) {
                        const cardButton = this.activeElement.querySelector('button');
                        if (cardButton) {
                            console.log('[Gamepad] Add Machine: clicking button in card');
                            this.resetButtonStates();
                            cardButton.click();
                            return;
                        }
                    }
                }
                
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
            
            // Check if we're in an OS dropdown option
            const isInOsDropdown = this.activeElement && 
                                 (this.activeElement.closest('[x-show*="osDropdowns"]') || 
                                  this.activeElement.closest('[x-show*="osDropdown"]'));
            
            // Also check for visible OS dropdown options anywhere
            const visibleOsDropdowns = document.querySelectorAll('div[x-show*="osDropdowns"]:not([x-cloak])')
                .length > 0;
                
            if (isInOsDropdown || visibleOsDropdowns) {
                console.log('[Gamepad] B pressed while in OS dropdown, closing dropdown');
                
                // Store a reference to the dropdown trigger before closing
                let dropdownTrigger = null;
                
                // If we have an active element in a dropdown, try to find its parent row
                if (this.activeElement && isInOsDropdown) {
                    const dropdownContainer = this.activeElement.closest('[x-show*="osDropdowns"]');
                    const parentRow = dropdownContainer?.closest('tr[data-machine-id]');
                    
                    // Try to find the dropdown trigger in this row
                    if (parentRow) {
                        dropdownTrigger = parentRow.querySelector('.cursor-pointer');
                        console.log('[Gamepad] Found dropdown trigger to return focus to:', dropdownTrigger);
                    }
                }
                
                // Close the dropdown by clicking the body (outside click)
                document.body.click();
                
                // Reset button states so we can continue using B button
                this.buttonStates.b = false;
                
                // After closing, update focusable elements and return focus to the trigger
                setTimeout(() => {
                    this.updateFocusableElements();
                    
                    // Return focus to the dropdown trigger if we found it
                    if (dropdownTrigger && this.focusableElements.includes(dropdownTrigger)) {
                        const triggerIndex = this.focusableElements.indexOf(dropdownTrigger);
                        console.log('[Gamepad] Returning focus to dropdown trigger at index:', triggerIndex);
                        this.focusElementAtIndex(triggerIndex);
                    }
                }, 100);
                
                return;
            }
            
            // Enhanced modal detection - similar to what we use elsewhere
            let modalVisible = null;
            const possibleModals = document.querySelectorAll('div[aria-modal="true"], div[id="add-machine-modal"], div[class*="modal"]');
            
            for (const modal of possibleModals) {
                if (modal.hasAttribute('x-cloak') || 
                    window.getComputedStyle(modal).display === 'none' ||
                    window.getComputedStyle(modal).visibility === 'hidden') {
                    continue;
                }
                
                const rect = modal.getBoundingClientRect();
                if (rect.width > 0 && rect.height > 0) {
                    modalVisible = modal;
                    break;
                }
            }
            
            if (modalVisible) {
                // Special handling for Add Machine modal
                if (modalVisible.id === 'add-machine-modal' || 
                    modalVisible.classList.contains('add-machine-modal') || 
                    modalVisible.querySelector('[x-show="addMachineModal"]')) {
                    
                    console.log('[Gamepad] B pressed in Add Machine modal');
                    // For Add Machine modal, find the cancel button by multiple methods
                    const cancelButton = modalVisible.querySelector(
                        'button[x-on\\:click*="addMachineModal = false"], ' +
                        'button[x-on\\:click*="false"], ' +
                        'button[x-on\\:click*="close"], ' +
                        'button.cancel, ' +
                        'button:last-child, ' +
                        'button[type="button"]:not([type="submit"])'
                    );
                    
                    if (cancelButton) {
                        console.log('[Gamepad] Found cancel button in Add Machine modal, clicking it');
                        cancelButton.click();
                    } else {
                        console.log('[Gamepad] No cancel button found in Add Machine modal, using Alpine.js global state');
                        // Try to use Alpine.js global state to close the modal
                        if (window.Alpine) {
                            console.log('[Gamepad] Attempting to close Add Machine modal via Alpine global state');
                            window.Alpine.store('app', { addMachineModal: false });
                            
                            // Also try document level Alpine data
                            document.querySelectorAll('[x-data]').forEach(el => {
                                try {
                                    const data = window.Alpine.evaluate(el, 'addMachineModal = false');
                                    console.log('[Gamepad] Alpine evaluation result:', data);
                                } catch (e) {
                                    // Ignore errors
                                }
                            });
                        }
                        
                        // Last resort: dispatch Escape key
                        console.log('[Gamepad] Last resort: dispatching Escape key');
                        document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', code: 'Escape', keyCode: 27, which: 27 }));
                    }
                } else {
                    // Regular modal handling
                    // Try to find a cancel button in the modal
                    const cancelButton = modalVisible.querySelector('button[type="button"]:last-child, button:last-child, .text-gray-500, [x-on\\:click*="false"], [x-on\\:click*="close"], button:not([type="submit"])');
                    if (cancelButton) {
                        console.log('[Gamepad] B pressed - closing modal by clicking cancel button');
                        cancelButton.click();
                    } else {
                        // Fallback: Just click outside the modal
                        console.log('[Gamepad] B pressed in modal, no cancel button found, trying outside click');
                        document.body.click();
                    }
                }
            } else {
                console.log('[Gamepad] B pressed - going back');
                window.history.back();
            }
        }
        
        // D-pad Up
        if (gamepad.buttons[12]?.pressed && !this.buttonStates.up) {
            console.log('[Gamepad] D-Pad Up PRESSED (detected new press).');
            this.buttonStates.up = true;
            // Ensure we have a focus element before navigating
            this.ensureFocus();
            this.navigateUp();
        }
        // D-pad Down
        if (gamepad.buttons[13]?.pressed && !this.buttonStates.down) {
            console.log('[Gamepad] D-Pad Down PRESSED (detected new press).');
            this.buttonStates.down = true;
            // Ensure we have a focus element before navigating
            this.ensureFocus();
            this.navigateDown();
        }
        // D-pad Left
        if (gamepad.buttons[14]?.pressed && !this.buttonStates.left) {
            console.log('[Gamepad] D-Pad Left PRESSED (detected new press).');
            this.buttonStates.left = true;
            // Ensure we have a focus element before navigating
            this.ensureFocus();
            this.navigateLeft();
        }
        // D-pad Right
        if (gamepad.buttons[15]?.pressed && !this.buttonStates.right) {
            console.log('[Gamepad] D-Pad Right PRESSED (detected new press).');
            this.buttonStates.right = true;
            // Ensure we have a focus element before navigating
            this.ensureFocus();
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
            console.log(`[Gamepad] Navigating UP from index ${this.currentElementIndex} to ${prevIndex}`);
            this.focusElementAtIndex(prevIndex);
        } else {
            console.log('[Gamepad] No element found above current position');
        }
    }
    
    navigateDown() {
        const nextIndex = this.findAdjacentElement('down');
        if (nextIndex !== -1 && nextIndex !== this.currentElementIndex) {
            console.log(`[Gamepad] Navigating DOWN from index ${this.currentElementIndex} to ${nextIndex}`);
            this.focusElementAtIndex(nextIndex);
        } else {
            console.log('[Gamepad] No element found below current position');
        }
    }
    
    navigateLeft() {
        const leftIndex = this.findAdjacentElement('left');
        if (leftIndex !== -1 && leftIndex !== this.currentElementIndex) {
            console.log(`[Gamepad] Navigating LEFT from index ${this.currentElementIndex} to ${leftIndex}`);
            this.focusElementAtIndex(leftIndex);
        } else {
            console.log('[Gamepad] No element found to the left of current position');
            
            // Special case: if we're on a Tags button, try to find an OS dropdown to focus
            if (this.activeElement && 
                this.activeElement.textContent && 
                this.activeElement.textContent.includes('Tags')) {
                
                // Find the parent row
                const parentRow = this.activeElement.closest('tr[data-machine-id]');
                if (parentRow) {
                    // Look for the OS dropdown trigger in this row
                    const osDropdownTrigger = parentRow.querySelector('.cursor-pointer');
                    if (osDropdownTrigger && this.focusableElements.includes(osDropdownTrigger)) {
                        const triggerIndex = this.focusableElements.indexOf(osDropdownTrigger);
                        console.log(`[Gamepad] Special case: Moving from Tags to OS dropdown at index ${triggerIndex}`);
                        this.focusElementAtIndex(triggerIndex);
                    }
                }
            }
        }
    }
    
    navigateRight() {
        const rightIndex = this.findAdjacentElement('right');
        if (rightIndex !== -1 && rightIndex !== this.currentElementIndex) {
            console.log(`[Gamepad] Navigating RIGHT from index ${this.currentElementIndex} to ${rightIndex}`);
            this.focusElementAtIndex(rightIndex);
        } else {
            console.log('[Gamepad] No element found to the right of current position');
        }
    }
    
    findAdjacentElement(direction) {
        if (!this.activeElement || this.focusableElements.length <= 1) {
            // If no active element or only one element, just focus the first one
            return this.focusableElements.length > 0 ? 0 : -1;
        }
        
        // If we're in a modal, only navigate between elements in that modal
        // Use improved modal detection
        let modalVisible = null;
        const possibleModals = document.querySelectorAll('div[aria-modal="true"], div[id="add-machine-modal"], div[class*="modal"]');
        
        for (const modal of possibleModals) {
            if (modal.hasAttribute('x-cloak') || 
                window.getComputedStyle(modal).display === 'none' ||
                window.getComputedStyle(modal).visibility === 'hidden') {
                continue;
            }
            
            const rect = modal.getBoundingClientRect();
            if (rect.width > 0 && rect.height > 0) {
                modalVisible = modal;
                break;
            }
        }
        
        const currentInModal = modalVisible && modalVisible.contains(this.activeElement);
        
        // If in modal, use standard navigation logic
        if (currentInModal) {
            return this.findAdjacentElementStandard(direction, modalVisible);
        }
        
        // Special grid-based navigation when no modal is active
        const currentRect = this.activeElement.getBoundingClientRect();
        const currentCenterX = currentRect.left + currentRect.width / 2;
        const currentCenterY = currentRect.top + currentRect.height / 2;
        
        // Check if current element is a row or an action button
        const isMachineRow = this.activeElement.tagName === 'TR' && 
                         this.activeElement.hasAttribute('data-machine-id');
        
        // Check if it's an Add Machine button
        const isAddMachineButton = this.activeElement.textContent && 
                               this.activeElement.textContent.trim().includes('Add Machine');
        
        // Check if it's an action button (inside a row)
        const isActionButton = (this.activeElement.tagName === 'BUTTON' || 
                            (this.activeElement.tagName === 'A' && this.activeElement.hasAttribute('href'))) && 
                            this.activeElement.closest('tr[data-machine-id]');
                            
        // Check if this is an OS dropdown trigger
        const isOsDropdownTrigger = this.activeElement.classList.contains('cursor-pointer') && 
                              (this.activeElement.getAttribute('x-on:click')?.includes('toggleOsDropdown') ||
                               this.activeElement.getAttribute('@click')?.includes('toggleOsDropdown') ||
                               this.activeElement.getAttribute('x-on:click')?.includes('osDropdowns') ||
                               this.activeElement.getAttribute('@click')?.includes('osDropdowns'));
        
        // Check if this is an OS dropdown option (inside a dropdown)
        const isOsDropdownOption = this.activeElement.closest('[x-show*="osDropdowns"]') && 
                             this.activeElement.tagName === 'A' && 
                             this.activeElement.getAttribute('href') === '#';
        
        // Check if current element is a Tags button
        const isTagsButton = this.activeElement.textContent && 
                          this.activeElement.textContent.includes('Tags');
                          
        // Check if current element is Reimage or Apply button
        const isReimageButton = this.activeElement.textContent && 
                             this.activeElement.textContent.includes('Reimage');
        const isApplyButton = this.activeElement.textContent && 
                           this.activeElement.textContent.includes('Apply');
        
        console.log(`[Gamepad] Navigation context: isMachineRow=${isMachineRow}, isAddMachineButton=${isAddMachineButton}, isActionButton=${isActionButton}, isOsDropdownTrigger=${isOsDropdownTrigger}, isOsDropdownOption=${isOsDropdownOption}, isTagsButton=${isTagsButton}, isReimageButton=${isReimageButton}, isApplyButton=${isApplyButton}, direction=${direction}`);
        
        // SPECIAL CASE: Direct row-to-row navigation when pressing up/down while on a row
        if (isMachineRow && (direction === 'up' || direction === 'down')) {
            console.log('[Gamepad] Special case: Row-to-row vertical navigation');
            // Get the adjacent row
            const adjacentRow = direction === 'down' 
                ? this.activeElement.nextElementSibling 
                : this.activeElement.previousElementSibling;
            
            // If the adjacent row is valid and in our focusable elements, focus it
            if (adjacentRow && 
                adjacentRow.tagName === 'TR' && 
                adjacentRow.hasAttribute('data-machine-id') && 
                this.focusableElements.includes(adjacentRow)) {
                
                const rowIndex = this.focusableElements.indexOf(adjacentRow);
                console.log(`[Gamepad] Found adjacent row at index ${rowIndex}`);
                return rowIndex;
            }
            
            // If we couldn't find an adjacent row, use standard navigation
            return this.findAdjacentElementStandard(direction);
        }
        
        // Special case for OS dropdown trigger when navigating left - go to row
        if (isOsDropdownTrigger && direction === 'left') {
            // Get the parent row
            const parentRow = this.activeElement.closest('tr[data-machine-id]');
            if (parentRow && this.focusableElements.includes(parentRow)) {
                console.log('[Gamepad] Special case: Moving from OS dropdown to row');
                return this.focusableElements.indexOf(parentRow);
            }
        }
        
        // Special case for Tags button when navigating left - go to OS dropdown
        if (isTagsButton && direction === 'left') {
            // Get the parent row
            const parentRow = this.activeElement.closest('tr[data-machine-id]');
            if (parentRow) {
                // Find the OS dropdown trigger
                const osDropdownTrigger = parentRow.querySelector('.cursor-pointer');
                if (osDropdownTrigger && this.focusableElements.includes(osDropdownTrigger)) {
                    console.log('[Gamepad] Special case: Moving from Tags button to OS dropdown');
                    return this.focusableElements.indexOf(osDropdownTrigger);
                }
            }
        }
        
        // Special case for vertical navigation between equivalent elements in different rows
        if ((direction === 'up' || direction === 'down') &&
            (isOsDropdownTrigger || isTagsButton || isReimageButton || isApplyButton)) {
            
            // FIRST CHECK: If on a dropdown trigger and pressing down, check if dropdown is open
            if (isOsDropdownTrigger && direction === 'down') {
                // Get the machine ID from the row containing this trigger
                const parentRow = this.activeElement.closest('tr[data-machine-id]');
                if (!parentRow) return this.currentElementIndex;
                
                const machineId = parentRow.getAttribute('data-machine-id');
                console.log(`[Gamepad] On OS dropdown trigger for machine ${machineId}, checking if dropdown is open`);
                
                // Check if the dropdown is open by looking for visible dropdown options
                const dropdownOptions = Array.from(document.querySelectorAll(`div[x-show*="osDropdowns"] a[href="#"]`))
                    .filter(option => {
                        const style = window.getComputedStyle(option);
                        return style.display !== 'none' && style.visibility !== 'hidden';
                    });
                
                if (dropdownOptions.length > 0) {
                    console.log(`[Gamepad] Found ${dropdownOptions.length} visible dropdown options`);
                    
                    // Find first option that's in our focusable elements
                    const firstOptionIndex = this.focusableElements.findIndex(el => 
                        dropdownOptions.includes(el)
                    );
                    
                    if (firstOptionIndex !== -1) {
                        console.log(`[Gamepad] Moving to first dropdown option at index ${firstOptionIndex}`);
                        return firstOptionIndex;
                    }
                } else {
                    console.log('[Gamepad] No visible dropdown options found, dropdown is closed');
                }
            }
            
            // If dropdown is closed (or we're on a different button type), continue with row-to-row navigation
            // Get current row
            const currentRow = this.activeElement.closest('tr[data-machine-id]');
            if (!currentRow) return this.findAdjacentElementStandard(direction);
            
            // Get adjacent row (next row for down, previous row for up)
            const adjacentRow = direction === 'down' 
                ? currentRow.nextElementSibling 
                : currentRow.previousElementSibling;
                
            if (!adjacentRow || !adjacentRow.hasAttribute('data-machine-id')) {
                return this.findAdjacentElementStandard(direction);
            }
            
            console.log(`[Gamepad] Looking for equivalent element in ${direction} row`);
            
            // Try to find equivalent element in adjacent row
            let targetElement = null;
            
            if (isOsDropdownTrigger) {
                // Find OS dropdown in adjacent row
                targetElement = adjacentRow.querySelector('.cursor-pointer');
                console.log('[Gamepad] Looking for OS dropdown trigger:', targetElement);
            }
            else if (isTagsButton) {
                // Find Tags button in adjacent row
                const buttons = Array.from(adjacentRow.querySelectorAll('button, a'));
                targetElement = buttons.find(btn => 
                    btn.textContent && btn.textContent.includes('Tags')
                );
                console.log('[Gamepad] Looking for Tags button:', targetElement);
            }
            else if (isReimageButton) {
                // From Reimage, look for Reimage or Apply (prefer Apply)
                const buttons = Array.from(adjacentRow.querySelectorAll('button, a'));
                
                // First check for Apply button
                targetElement = buttons.find(btn => 
                    btn.textContent && btn.textContent.includes('Apply')
                );
                
                // If no Apply found, look for Reimage
                if (!targetElement) {
                    targetElement = buttons.find(btn => 
                        btn.textContent && btn.textContent.includes('Reimage')
                    );
                }
                
                console.log('[Gamepad] From Reimage, looking for Apply/Reimage button:', targetElement);
            }
            else if (isApplyButton) {
                // From Apply, look for Apply or Reimage (prefer Apply)
                const buttons = Array.from(adjacentRow.querySelectorAll('button, a'));
                
                // First check for Apply button
                targetElement = buttons.find(btn => 
                    btn.textContent && btn.textContent.includes('Apply')
                );
                
                // If no Apply found, look for Reimage
                if (!targetElement) {
                    targetElement = buttons.find(btn => 
                        btn.textContent && btn.textContent.includes('Reimage')
                    );
                }
                
                console.log('[Gamepad] From Apply, looking for Apply/Reimage button:', targetElement);
            }
            
            // If we found a matching element that's focusable, use it
            if (targetElement && this.focusableElements.includes(targetElement)) {
                const targetIndex = this.focusableElements.indexOf(targetElement);
                console.log(`[Gamepad] Found equivalent element in ${direction} row at index ${targetIndex}`);
                return targetIndex;
            }
            
            // If we didn't find an equivalent, just focus the row
            if (this.focusableElements.includes(adjacentRow)) {
                const rowIndex = this.focusableElements.indexOf(adjacentRow);
                console.log(`[Gamepad] No equivalent found, focusing ${direction} row at index ${rowIndex}`);
                return rowIndex;
            }
        }
        
        // If on a dropdown option and pressing up/down, navigate between options
        if (isOsDropdownOption && (direction === 'up' || direction === 'down')) {
            // Get all visible options in this dropdown
            const dropdown = this.activeElement.closest('[x-show*="osDropdowns"]');
            if (!dropdown) return this.currentElementIndex;
            
            const allOptions = Array.from(dropdown.querySelectorAll('a[href="#"]'))
                .filter(option => {
                    const style = window.getComputedStyle(option);
                    return style.display !== 'none' && style.visibility !== 'hidden' && 
                          this.focusableElements.includes(option);
                });
            
            // Find current option index
            const currentOptionIndex = allOptions.indexOf(this.activeElement);
            if (currentOptionIndex === -1) return this.currentElementIndex;
            
            if (direction === 'up') {
                // If at first option and going up, go back to dropdown trigger
                if (currentOptionIndex === 0) {
                    const trigger = dropdown.closest('tr').querySelector('.cursor-pointer');
                    if (trigger && this.focusableElements.includes(trigger)) {
                        return this.focusableElements.indexOf(trigger);
                    }
                }
                // Otherwise go to previous option
                else {
                    return this.focusableElements.indexOf(allOptions[currentOptionIndex - 1]);
                }
            } else { // down
                // If at last option, close dropdown and go to next row
                if (currentOptionIndex === allOptions.length - 1) {
                    // Close dropdown programmatically
                    document.body.click();
                    
                    // Find next row
                    const currentRow = dropdown.closest('tr');
                    if (currentRow) {
                        const nextRow = currentRow.nextElementSibling;
                        if (nextRow && nextRow.hasAttribute('data-machine-id') && 
                            this.focusableElements.includes(nextRow)) {
                            return this.focusableElements.indexOf(nextRow);
                        }
                    }
                }
                // Otherwise go to next option
                else {
                    return this.focusableElements.indexOf(allOptions[currentOptionIndex + 1]);
                }
            }
            
            // If we get here, we couldn't navigate within options
            return this.currentElementIndex;
        }
        
        // For vertical navigation (up/down), only consider rows and Add Machine button
        if (direction === 'up' || direction === 'down') {
            // If on a dropdown option and pressing up/down, navigate between options
            if (isOsDropdownOption) {
                // ... existing code ...
            }
        }
        
        // For horizontal navigation (left/right)
        else if (direction === 'left' || direction === 'right') {
            // If on a row, only find action buttons within that row
            if (isMachineRow) {
                // Log to help debug
                console.log('[Gamepad] Looking for action buttons in row:', this.activeElement);
                
                // Get all action buttons in this row - the issue is these might not be direct children
                let actionButtons = [];
                
                // CUSTOM DOM TRAVERSAL: Since the buttons aren't in cells, but directly within row elements
                // and might be in a div container, we need a more comprehensive search
                
                // First try to find buttons with Alpine click handlers (@click)
                const rowButtons = Array.from(this.activeElement.querySelectorAll('[class*="inline-flex"], [class*="rounded"], [class*="py-1"], [class*="px-3"]'));
                console.log(`[Gamepad] Found ${rowButtons.length} potential action buttons with button styling`);
                
                if (rowButtons.length > 0) {
                    // Filter to likely action buttons
                    actionButtons = rowButtons.filter(btn => {
                        // Check various properties that suggest this is an action button
                        const hasClickHandler = btn.hasAttribute('@click') || 
                                               btn.hasAttribute('x-on:click') ||
                                               btn.hasAttribute('v-on:click') ||
                                               btn.hasAttribute('ng-click') ||
                                               btn.hasAttribute('data-action');
                                               
                        const isActionLike = btn.classList.contains('rounded') || 
                                           btn.classList.contains('btn') || 
                                           btn.classList.contains('button') ||
                                           btn.tagName === 'BUTTON' ||
                                           (btn.tagName === 'A' && btn.hasAttribute('href'));
                                           
                        return isActionLike || hasClickHandler;
                    });
                }
                
                // If that failed, try for any buttons or links
                if (actionButtons.length === 0) {
                    // Look for any buttons or links within the row
                    actionButtons = Array.from(this.activeElement.querySelectorAll('button, a[href]'));
                    console.log(`[Gamepad] Found ${actionButtons.length} basic buttons and links`);
                }
                
                // If that failed too, look for tag buttons specifically
                if (actionButtons.length === 0) {
                    const allElements = Array.from(this.activeElement.querySelectorAll('*'));
                    actionButtons = allElements.filter(el => {
                        const text = el.textContent?.trim();
                        return text === '🏷️ Tags' || text === '♻️ Reimage' || text === '⏻ Power';
                    });
                    console.log(`[Gamepad] Found ${actionButtons.length} buttons by text content`);
                }
                
                // Filter to only include buttons that are focusable and not excluded
                actionButtons = actionButtons.filter(button => {
                    // Basic visibility check
                    try {
                        const style = window.getComputedStyle(button);
                        if (style.display === 'none' || style.visibility === 'hidden') {
                            return false;
                        }
                    } catch (e) {
                        return false;
                    }
                    
                    // Check if it's already in focusable elements
                    const isIncluded = this.focusableElements.includes(button);
                    
                    // If not included yet, check if we should add it
                    if (!isIncluded) {
                        // Add the button to focusable elements if it looks like a real button
                        const isRealButton = 
                            button.tagName === 'BUTTON' || 
                            button.tagName === 'A' || 
                            button.getAttribute('role') === 'button' ||
                            button.classList.contains('btn') ||
                            button.classList.contains('button') ||
                            button.classList.contains('rounded');
                            
                        if (isRealButton && !button.classList.contains('gamepad-nav-exclude')) {
                            console.log('[Gamepad] Adding button to focusable elements:', button);
                            this.focusableElements.push(button);
                            return true;
                        }
                        return false;
                    }
                    
                    // Check excluded flag
                    const isExcluded = button.classList.contains('gamepad-nav-exclude');
                    console.log(`[Gamepad] Button ${button.textContent?.trim() || button}: included=${isIncluded}, excluded=${isExcluded}`);
                    return isIncluded && !isExcluded;
                });
                
                console.log(`[Gamepad] Found ${actionButtons.length} final action buttons in this row`);
                
                if (actionButtons.length === 0) {
                    console.log('[Gamepad] No action buttons found in this row, staying on row');
                    return this.currentElementIndex; // No action buttons to navigate to
                }
                
                // If on an action button, navigate between buttons or back to row
                if (isActionButton) {
                    const parentRow = this.activeElement.closest('tr[data-machine-id]');
                    if (!parentRow) {
                        return this.currentElementIndex; // Shouldn't happen
                    }
                    
                    // For Tags button or any button with "Tags" text, always go back to row when pressing left
                    if (direction === 'left' && 
                        this.activeElement.textContent && 
                        this.activeElement.textContent.includes('Tags')) {
                        console.log('[Gamepad] Tags button detected, going back to row');
                        return this.focusableElements.indexOf(parentRow);
                    }
                    
                    // Get all action buttons in this row
                    const actionButtons = Array.from(parentRow.querySelectorAll('button, a[href], [class*="rounded"], [class*="inline-flex"]'))
                        .filter(button => {
                            return this.focusableElements.includes(button) && 
                                   !button.classList.contains('gamepad-nav-exclude');
                        });
                    
                    if (actionButtons.length <= 1) {
                        // Only one button, navigate back to the row
                        console.log('[Gamepad] Only one action button, returning to row');
                        const rowIndex = this.focusableElements.indexOf(parentRow);
                        return rowIndex !== -1 ? rowIndex : this.currentElementIndex;
                    }
                    
                    // Sort buttons by their position from left to right
                    actionButtons.sort((a, b) => {
                        const rectA = a.getBoundingClientRect();
                        const rectB = b.getBoundingClientRect();
                        return rectA.left - rectB.left;
                    });
                    
                    // Find current button index in the buttons array
                    const currentButtonIndex = actionButtons.indexOf(this.activeElement);
                    console.log(`[Gamepad] Current button index: ${currentButtonIndex} of ${actionButtons.length}`);
                    
                    if (currentButtonIndex === -1) {
                        console.log('[Gamepad] Button not found in sorted array, returning to row');
                        return this.focusableElements.indexOf(parentRow);
                    }
                    
                    // Navigate to next/previous button
                    if (direction === 'right') {
                        // If at the last button, go back to row
                        if (currentButtonIndex === actionButtons.length - 1) {
                            console.log('[Gamepad] At last button, returning to row');
                            return this.focusableElements.indexOf(parentRow);
                        }
                        // Otherwise go to next button
                        console.log(`[Gamepad] Moving to next button: ${currentButtonIndex + 1}`);
                        return this.focusableElements.indexOf(actionButtons[currentButtonIndex + 1]);
                    } else {
                        // If at the first button, go back to row
                        if (currentButtonIndex === 0) {
                            console.log('[Gamepad] At first button, returning to row');
                            return this.focusableElements.indexOf(parentRow);
                        }
                        // Otherwise go to previous button
                        console.log(`[Gamepad] Moving to previous button: ${currentButtonIndex - 1}`);
                        return this.focusableElements.indexOf(actionButtons[currentButtonIndex - 1]);
                    }
                }
                // Add Machine button or other element - fall back to standard navigation
                else {
                    return this.findAdjacentElementStandard(direction);
                }
            }
            // If on an action button, navigate between buttons or back to row
            else if (isActionButton) {
                const parentRow = this.activeElement.closest('tr[data-machine-id]');
                if (!parentRow) {
                    return this.currentElementIndex; // Shouldn't happen
                }
                
                // For Tags button or any button with "Tags" text, always go back to row when pressing left
                if (direction === 'left' && 
                    this.activeElement.textContent && 
                    this.activeElement.textContent.includes('Tags')) {
                    console.log('[Gamepad] Tags button detected, going back to row');
                    return this.focusableElements.indexOf(parentRow);
                }
                
                // Get all action buttons in this row
                const actionButtons = Array.from(parentRow.querySelectorAll('button, a[href], [class*="rounded"], [class*="inline-flex"]'))
                    .filter(button => {
                        return this.focusableElements.includes(button) && 
                               !button.classList.contains('gamepad-nav-exclude');
                    });
                
                if (actionButtons.length <= 1) {
                    // Only one button, navigate back to the row
                    console.log('[Gamepad] Only one action button, returning to row');
                    const rowIndex = this.focusableElements.indexOf(parentRow);
                    return rowIndex !== -1 ? rowIndex : this.currentElementIndex;
                }
                
                // Sort buttons by their position from left to right
                actionButtons.sort((a, b) => {
                    const rectA = a.getBoundingClientRect();
                    const rectB = b.getBoundingClientRect();
                    return rectA.left - rectB.left;
                });
                
                // Find current button index in the buttons array
                const currentButtonIndex = actionButtons.indexOf(this.activeElement);
                console.log(`[Gamepad] Current button index: ${currentButtonIndex} of ${actionButtons.length}`);
                
                if (currentButtonIndex === -1) {
                    console.log('[Gamepad] Button not found in sorted array, returning to row');
                    return this.focusableElements.indexOf(parentRow);
                }
                
                // Navigate to next/previous button
                if (direction === 'right') {
                    // If at the last button, go back to row
                    if (currentButtonIndex === actionButtons.length - 1) {
                        console.log('[Gamepad] At last button, returning to row');
                        return this.focusableElements.indexOf(parentRow);
                    }
                    // Otherwise go to next button
                    console.log(`[Gamepad] Moving to next button: ${currentButtonIndex + 1}`);
                    return this.focusableElements.indexOf(actionButtons[currentButtonIndex + 1]);
                } else {
                    // If at the first button, go back to row
                    if (currentButtonIndex === 0) {
                        console.log('[Gamepad] At first button, returning to row');
                        return this.focusableElements.indexOf(parentRow);
                    }
                    // Otherwise go to previous button
                    console.log(`[Gamepad] Moving to previous button: ${currentButtonIndex - 1}`);
                    return this.focusableElements.indexOf(actionButtons[currentButtonIndex - 1]);
                }
            }
            // Add Machine button or other element - fall back to standard navigation
            else {
                return this.findAdjacentElementStandard(direction);
            }
        }
        
        // If we get here, use standard navigation
        return this.findAdjacentElementStandard(direction);
    }
    
    // Original adjacent element finding logic, extracted to a method
    findAdjacentElementStandard(direction, modalElement = null) {
        const currentRect = this.activeElement.getBoundingClientRect();
        const currentCenterX = currentRect.left + currentRect.width / 2;
        const currentCenterY = currentRect.top + currentRect.height / 2;
        
        let bestIndex = -1;
        let bestDistance = Number.POSITIVE_INFINITY;
        
        // Check all focusable elements
        this.focusableElements.forEach((element, index) => {
            if (element === this.activeElement) return;
            
            // If we're in a modal, only navigate to elements also in that modal
            if (modalElement && !modalElement.contains(element)) {
                return;
            }
            
            // Make sure the element is actually visible
            const rect = element.getBoundingClientRect();
            if (rect.width === 0 || rect.height === 0) return;
            
            const centerX = rect.left + rect.width / 2;
            const centerY = rect.top + rect.height / 2;
            
            // Check if element is in the right direction
            let inRightDirection = false;
            switch (direction) {
                case 'up':
                    inRightDirection = centerY < currentCenterY - 10; // Must be significantly above
                    break;
                case 'down':
                    inRightDirection = centerY > currentCenterY + 10; // Must be significantly below
                    break;
                case 'left':
                    inRightDirection = centerX < currentCenterX - 10; // Must be significantly to the left
                    break;
                case 'right':
                    inRightDirection = centerX > currentCenterX + 10; // Must be significantly to the right
                    break;
            }
            
            if (inRightDirection) {
                // Calculate distance based on direction priority
                let distance;
                
                if (direction === 'up' || direction === 'down') {
                    // For up/down prioritize vertical distance much more strongly
                    distance = Math.abs(centerY - currentCenterY) * 0.8 + Math.abs(centerX - currentCenterX) * 5;
                } else {
                    // For left/right prioritize horizontal distance much more strongly
                    distance = Math.abs(centerX - currentCenterX) * 0.8 + Math.abs(centerY - currentCenterY) * 5;
                }
                
                if (distance < bestDistance) {
                    bestDistance = distance;
                    bestIndex = index;
                }
            }
        });
        
        return bestIndex !== -1 ? bestIndex : this.currentElementIndex;
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
    
    // Method to ensure we have an active focus element
    ensureFocus() {
        // If we don't have an active element but we have focusable elements
        if (!this.activeElement && this.focusableElements.length > 0) {
            console.log('[Gamepad] No active element, setting focus to first element');
            this.focusElementAtIndex(0);
            return true;
        }
        
        // Check if the active element is still in the document
        if (this.activeElement && !document.body.contains(this.activeElement)) {
            console.log('[Gamepad] Active element is no longer in the document, resetting focus');
            this.activeElement = null;
            this.focusElementAtIndex(0);
            return true;
        }
        
        return false;
    }
}

// Initialize the gamepad controller when the script loads
document.addEventListener('DOMContentLoaded', () => {
    // Delay initialization slightly to potentially help with element finding
    setTimeout(() => { 
        console.log('[Gamepad] Initializing gamepad controller...');
        
        // Check if controller already exists
        if (!window.gamepadController) {
            window.gamepadController = new GamepadController();
            
            // Force navigation update after a delay to ensure DOM is fully ready
            setTimeout(() => {
                if (window.gamepadController) {
                    console.log('[Gamepad] Refreshing navigation elements...');
                    window.gamepadController.updateFocusableElements();
                    
                    // Try to focus on first machine row if on machine list page
                    const machineRows = document.querySelectorAll('tr[data-machine-id]');
                    if (machineRows.length > 0) {
                        const firstRowIndex = window.gamepadController.focusableElements.findIndex(el => 
                            el.tagName === 'TR' && el.hasAttribute('data-machine-id')
                        );
                        
                        if (firstRowIndex !== -1) {
                            console.log('[Gamepad] Focusing first machine row on page load');
                            window.gamepadController.focusElementAtIndex(firstRowIndex);
                        } else if (window.gamepadController.focusableElements.length > 0) {
                            window.gamepadController.focusElementAtIndex(0);
                        }
                    } else if (window.gamepadController.focusableElements.length > 0) {
                        window.gamepadController.focusElementAtIndex(0);
                    }
                }
            }, 1000);
        }
    }, 300);
}); 