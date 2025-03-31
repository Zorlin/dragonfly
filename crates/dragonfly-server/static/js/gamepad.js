// Dragonfly Gamepad Navigation Controller
// Provides support for Xbox controllers and generic gamepad input
document.addEventListener('DOMContentLoaded', function() {
    // State management
    const gamepadState = {
        connected: false,
        activeGamepad: null,
        focusedElement: null,
        focusableElements: [],
        focusIndex: 0,
        // Button mappings based on Xbox controller layout
        buttons: {
            A: 0,       // Primary/Select
            B: 1,       // Back/Cancel
            X: 2,       // Secondary action
            Y: 3,       // Tertiary action
            LB: 4,      // Left bumper - previous section
            RB: 5,      // Right bumper - next section
            LT: 6,      // Left trigger - zoom out
            RT: 7,      // Right trigger - zoom in
            BACK: 8,    // Back button
            START: 9,   // Start button - open menu
            LS: 10,     // Left stick press
            RS: 11,     // Right stick press
            UP: 12,     // D-pad up
            DOWN: 13,   // D-pad down
            LEFT: 14,   // D-pad left
            RIGHT: 15   // D-pad right
        },
        // Axis mappings
        axes: {
            LS_X: 0,    // Left stick X axis
            LS_Y: 1,    // Left stick Y axis
            RS_X: 2,    // Right stick X axis
            RS_Y: 3     // Right stick Y axis
        },
        // Deadzone for analog sticks
        deadzone: 0.25
    };

    // Visual indicator for currently focused element
    function createFocusIndicator() {
        let indicator = document.getElementById('gamepad-focus-indicator');
        if (!indicator) {
            indicator = document.createElement('div');
            indicator.id = 'gamepad-focus-indicator';
            indicator.className = 'fixed pointer-events-none border-2 border-yellow-400 dark:border-yellow-300 rounded-lg shadow-lg opacity-0 transition-all duration-100 z-50';
            document.body.appendChild(indicator);
        }
        return indicator;
    }

    const focusIndicator = createFocusIndicator();

    // Find all focusable elements
    function refreshFocusableElements() {
        // Get all interactive elements that could receive focus
        const selector = 'a, button, [role="button"], [tabindex]:not([tabindex="-1"])';
        gamepadState.focusableElements = Array.from(document.querySelectorAll(selector))
            .filter(el => {
                // Filter out hidden elements
                const style = window.getComputedStyle(el);
                return style.display !== 'none' && 
                       style.visibility !== 'hidden' && 
                       style.opacity !== '0' &&
                       el.offsetWidth > 0 && 
                       el.offsetHeight > 0;
            });

        // If we had a focused element, try to find it again in the new list
        if (gamepadState.focusedElement) {
            const idx = gamepadState.focusableElements.indexOf(gamepadState.focusedElement);
            gamepadState.focusIndex = idx >= 0 ? idx : 0;
        }
    }

    // Update which element is focused
    function updateFocus(direction) {
        if (gamepadState.focusableElements.length === 0) {
            refreshFocusableElements();
            if (gamepadState.focusableElements.length === 0) return;
        }

        // Calculate new index based on direction
        let newIndex = gamepadState.focusIndex;
        if (direction === 'next') {
            newIndex = (gamepadState.focusIndex + 1) % gamepadState.focusableElements.length;
        } else if (direction === 'prev') {
            newIndex = (gamepadState.focusIndex - 1 + gamepadState.focusableElements.length) % gamepadState.focusableElements.length;
        } else if (typeof direction === 'number') {
            newIndex = direction;
        }

        // Update the focus
        gamepadState.focusIndex = newIndex;
        gamepadState.focusedElement = gamepadState.focusableElements[newIndex];
        
        // Focus the element
        gamepadState.focusedElement.focus();
        
        // Update the visual indicator
        updateFocusIndicator();
    }

    // Update the visual focus indicator
    function updateFocusIndicator() {
        if (!gamepadState.focusedElement) return;
        
        const rect = gamepadState.focusedElement.getBoundingClientRect();
        focusIndicator.style.left = `${rect.left - 4}px`;
        focusIndicator.style.top = `${rect.top - 4}px`;
        focusIndicator.style.width = `${rect.width + 8}px`;
        focusIndicator.style.height = `${rect.height + 8}px`;
        focusIndicator.style.opacity = '1';
    }

    // Find the closest element in a particular direction
    function findClosestInDirection(direction) {
        if (!gamepadState.focusedElement || gamepadState.focusableElements.length <= 1) return;
        
        const currentRect = gamepadState.focusedElement.getBoundingClientRect();
        const currentCenter = {
            x: currentRect.left + currentRect.width / 2,
            y: currentRect.top + currentRect.height / 2
        };
        
        // Calculate scores for each focusable element based on direction
        let bestScore = Number.MAX_SAFE_INTEGER;
        let bestIdx = -1;

        gamepadState.focusableElements.forEach((el, idx) => {
            if (el === gamepadState.focusedElement) return;
            
            const rect = el.getBoundingClientRect();
            const center = {
                x: rect.left + rect.width / 2,
                y: rect.top + rect.height / 2
            };
            
            const dx = center.x - currentCenter.x;
            const dy = center.y - currentCenter.y;
            const distance = Math.sqrt(dx * dx + dy * dy);
            
            // Score based on direction and distance
            let score = distance;
            
            // Heavily favor the correct direction
            switch (direction) {
                case 'up':
                    // For up direction, element must be above
                    if (dy >= -10) score = Number.MAX_SAFE_INTEGER;
                    else score = distance - dy * 2; // Prioritize elements more directly above
                    break;
                case 'down':
                    // For down direction, element must be below
                    if (dy <= 10) score = Number.MAX_SAFE_INTEGER;
                    else score = distance - dy * 2; // Prioritize elements more directly below
                    break;
                case 'left':
                    // For left direction, element must be to the left
                    if (dx >= -10) score = Number.MAX_SAFE_INTEGER;
                    else score = distance - dx * 2; // Prioritize elements more directly to the left
                    break;
                case 'right':
                    // For right direction, element must be to the right
                    if (dx <= 10) score = Number.MAX_SAFE_INTEGER;
                    else score = distance - dx * 2; // Prioritize elements more directly to the right
                    break;
            }
            
            // Update best score
            if (score < bestScore) {
                bestScore = score;
                bestIdx = idx;
            }
        });
        
        // If we found a good candidate, update focus
        if (bestIdx !== -1) {
            updateFocus(bestIdx);
        }
    }

    // Handle button press
    function handleButtonPress(buttonIndex) {
        if (!gamepadState.focusedElement) return;
        
        switch (buttonIndex) {
            case gamepadState.buttons.A:
                // Press the currently focused element
                gamepadState.focusedElement.click();
                break;
                
            case gamepadState.buttons.B:
                // Go back to previous page
                history.back();
                break;
                
            case gamepadState.buttons.UP:
                findClosestInDirection('up');
                break;
                
            case gamepadState.buttons.DOWN:
                findClosestInDirection('down');
                break;
                
            case gamepadState.buttons.LEFT:
                findClosestInDirection('left');
                break;
                
            case gamepadState.buttons.RIGHT:
                findClosestInDirection('right');
                break;
                
            case gamepadState.buttons.LB:
                // Previous tab/section
                break;
                
            case gamepadState.buttons.RB:
                // Next tab/section
                break;
                
            case gamepadState.buttons.START:
                // Toggle menu
                break;
        }
    }

    // Handle stick movement
    function handleStickMovement(sticks) {
        // Left stick directional navigation with deadzone
        if (Math.abs(sticks.LS_Y) > gamepadState.deadzone || Math.abs(sticks.LS_X) > gamepadState.deadzone) {
            // Determine primary direction
            if (Math.abs(sticks.LS_Y) > Math.abs(sticks.LS_X)) {
                // Vertical movement
                if (sticks.LS_Y < -gamepadState.deadzone) {
                    findClosestInDirection('up');
                } else if (sticks.LS_Y > gamepadState.deadzone) {
                    findClosestInDirection('down');
                }
            } else {
                // Horizontal movement
                if (sticks.LS_X < -gamepadState.deadzone) {
                    findClosestInDirection('left');
                } else if (sticks.LS_X > gamepadState.deadzone) {
                    findClosestInDirection('right');
                }
            }
        }
        
        // Right stick for scrolling
        if (Math.abs(sticks.RS_Y) > gamepadState.deadzone) {
            window.scrollBy(0, sticks.RS_Y * 15);
        }
    }

    // Check for gamepad input
    let lastButtonStates = [];
    let lastAxisValues = [];
    let lastMovementTime = 0;

    function checkGamepadInput() {
        const gamepads = navigator.getGamepads ? navigator.getGamepads() : [];
        let gamepad = null;
        
        // Find the first connected gamepad
        for (let i = 0; i < gamepads.length; i++) {
            if (gamepads[i] && gamepads[i].connected) {
                gamepad = gamepads[i];
                gamepadState.activeGamepad = gamepad;
                gamepadState.connected = true;
                break;
            }
        }
        
        if (!gamepad) {
            gamepadState.connected = false;
            gamepadState.activeGamepad = null;
            requestAnimationFrame(checkGamepadInput);
            return;
        }
        
        // Initialize button states if needed
        if (lastButtonStates.length === 0) {
            lastButtonStates = gamepad.buttons.map(b => b.pressed);
        }
        
        // Initialize axis values if needed
        if (lastAxisValues.length === 0) {
            lastAxisValues = gamepad.axes.map(a => a);
        }
        
        // Check buttons
        gamepad.buttons.forEach((button, index) => {
            if (button.pressed && !lastButtonStates[index]) {
                // Button just pressed
                handleButtonPress(index);
            }
            lastButtonStates[index] = button.pressed;
        });
        
        // Handle stick movement (throttled to prevent too fast navigation)
        const now = Date.now();
        if (now - lastMovementTime > 250) { // Throttle to every 250ms
            const sticks = {
                LS_X: gamepad.axes[gamepadState.axes.LS_X],
                LS_Y: gamepad.axes[gamepadState.axes.LS_Y],
                RS_X: gamepad.axes[gamepadState.axes.RS_X],
                RS_Y: gamepad.axes[gamepadState.axes.RS_Y]
            };
            
            // Check if any stick has moved significantly
            const significantMovement = Object.values(sticks).some(value => 
                Math.abs(value) > gamepadState.deadzone
            );
            
            if (significantMovement) {
                handleStickMovement(sticks);
                lastMovementTime = now;
            }
            
            // Update last axis values
            lastAxisValues = gamepad.axes.map(a => a);
        }
        
        // Continue checking for gamepad input
        requestAnimationFrame(checkGamepadInput);
    }

    // Initialize gamepad detection
    function initGamepadControl() {
        // Set up the focus indicator style
        const style = document.createElement('style');
        style.textContent = `
            @keyframes pulse {
                0% { opacity: 0.7; }
                50% { opacity: 0.9; }
                100% { opacity: 0.7; }
            }
            #gamepad-focus-indicator {
                animation: pulse 1.5s infinite;
            }
        `;
        document.head.appendChild(style);

        // Listen for gamepad connections
        window.addEventListener('gamepadconnected', (e) => {
            console.log(`Gamepad connected: ${e.gamepad.id}`);
            gamepadState.connected = true;
            gamepadState.activeGamepad = e.gamepad;
            document.body.classList.add('gamepad-active');
            
            // Initialize focus if not already set
            if (!gamepadState.focusedElement) {
                refreshFocusableElements();
                if (gamepadState.focusableElements.length > 0) {
                    updateFocus(0);
                }
            }
            
            // Start checking for gamepad input
            requestAnimationFrame(checkGamepadInput);
        });

        // Listen for gamepad disconnections
        window.addEventListener('gamepaddisconnected', (e) => {
            console.log(`Gamepad disconnected: ${e.gamepad.id}`);
            // If this was our active gamepad
            if (gamepadState.activeGamepad && gamepadState.activeGamepad.index === e.gamepad.index) {
                gamepadState.connected = false;
                gamepadState.activeGamepad = null;
                document.body.classList.remove('gamepad-active');
                
                // Hide the focus indicator
                focusIndicator.style.opacity = '0';
            }
        });
        
        // Listen for DOM changes to refresh focusable elements
        const observer = new MutationObserver(mutations => {
            refreshFocusableElements();
        });
        
        observer.observe(document.body, {
            childList: true,
            subtree: true,
            attributes: true,
            attributeFilter: ['class', 'style', 'hidden']
        });
        
        // Initial check for already-connected gamepads
        if (navigator.getGamepads) {
            const gamepads = navigator.getGamepads();
            for (let i = 0; i < gamepads.length; i++) {
                if (gamepads[i] && gamepads[i].connected) {
                    gamepadState.connected = true;
                    gamepadState.activeGamepad = gamepads[i];
                    document.body.classList.add('gamepad-active');
                    
                    // Initialize focus
                    refreshFocusableElements();
                    if (gamepadState.focusableElements.length > 0) {
                        updateFocus(0);
                    }
                    
                    // Start checking for gamepad input
                    requestAnimationFrame(checkGamepadInput);
                    break;
                }
            }
        }
        
        // Initial refresh of focusable elements
        refreshFocusableElements();
    }

    // Initialize the gamepad control system
    initGamepadControl();
}); 